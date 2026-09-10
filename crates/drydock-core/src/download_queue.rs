//! The persistent download queue's ordering rules, as pure functions over `Vec<QueuedDownload>`.
//!
//! The queue lives in the settings and drives which depot download runs: the **front entry is the
//! current one**. Reordering it is where the UI's download state machine is easiest to get wrong —
//! a `remove(0)` on an empty queue panics, a reorder that forgets to restart leaves nothing running,
//! and "send to back" with a single entry has no back to send it to.
//!
//! Those rules live here rather than in `ui.rs` so they can be tested without a window, a thread, or
//! a Steam install. Each function reports, via [`QueueEffect`], what the caller must do with the
//! worker thread afterwards; the functions themselves never touch threads or the filesystem.

use crate::settings::QueuedDownload;

/// What the caller must do to the running download after a queue change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueEffect {
    /// The queue is unchanged; do nothing (and do not persist).
    Unchanged,
    /// The front entry changed while a download was running: stop it and start the new front. The
    /// stopped entry keeps its place in the queue and resumes when it reaches the front again.
    SwitchToFront,
    /// Nothing is running and there is a front entry to start.
    StartFront,
    /// The queue changed but what is running (if anything) stays correct — just persist.
    PersistOnly,
}

/// Appends `entry` unless its App ID is already queued.
///
/// Returns [`QueueEffect::Unchanged`] for a duplicate so the caller can say "already queued" instead
/// of silently adding a second copy that would download the same game twice.
pub fn enqueue(queue: &mut Vec<QueuedDownload>, entry: QueuedDownload, running: bool) -> QueueEffect {
    if queue.iter().any(|item| item.app_id == entry.app_id) {
        return QueueEffect::Unchanged;
    }
    queue.push(entry);
    if running {
        QueueEffect::PersistOnly
    } else {
        QueueEffect::StartFront
    }
}

/// Drops the front (current) entry, e.g. after the user removes a paused download.
pub fn remove_front(queue: &mut Vec<QueuedDownload>) -> QueueEffect {
    if queue.is_empty() {
        return QueueEffect::Unchanged;
    }
    queue.remove(0);
    QueueEffect::StartFront
}

/// Drops a queued entry that is **not** the current one. The front is left alone so a running
/// download is never pulled out from under its worker thread.
pub fn remove_queued(queue: &mut Vec<QueuedDownload>, app_id: u32) -> QueueEffect {
    if queue.first().is_some_and(|front| front.app_id == app_id) {
        return QueueEffect::Unchanged;
    }
    let before = queue.len();
    queue.retain(|item| item.app_id != app_id);
    if queue.len() == before {
        QueueEffect::Unchanged
    } else {
        QueueEffect::PersistOnly
    }
}

/// Drops a completed download by App ID, wherever it sits.
///
/// Matching by App ID rather than position matters: the queue can have been reordered while the
/// download ran, so `remove(0)` could drop the wrong game.
pub fn remove_completed(queue: &mut Vec<QueuedDownload>, app_id: u32) -> QueueEffect {
    let before = queue.len();
    queue.retain(|item| item.app_id != app_id);
    if queue.len() == before {
        QueueEffect::Unchanged
    } else {
        QueueEffect::StartFront
    }
}

/// Moves `app_id` to the front so it becomes the current download.
pub fn activate(queue: &mut Vec<QueuedDownload>, app_id: u32, running: bool) -> QueueEffect {
    let Some(position) = queue.iter().position(|item| item.app_id == app_id) else {
        return QueueEffect::Unchanged;
    };
    if position == 0 && running {
        return QueueEffect::Unchanged; // already the active download
    }
    let item = queue.remove(position);
    queue.insert(0, item);
    if running {
        QueueEffect::SwitchToFront
    } else {
        QueueEffect::StartFront
    }
}

/// Sends the current download to the back of the queue.
///
/// With fewer than two entries there is no "back" to move to, so the caller should pause instead —
/// signalled by [`QueueEffect::Unchanged`].
pub fn demote_front(queue: &mut Vec<QueuedDownload>, running: bool) -> QueueEffect {
    if queue.len() < 2 {
        return QueueEffect::Unchanged;
    }
    let item = queue.remove(0);
    queue.push(item);
    if running {
        QueueEffect::SwitchToFront
    } else {
        QueueEffect::StartFront
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued(app_id: u32) -> QueuedDownload {
        QueuedDownload {
            app_id,
            name: format!("Game {app_id}"),
        }
    }

    fn ids(queue: &[QueuedDownload]) -> Vec<u32> {
        queue.iter().map(|item| item.app_id).collect()
    }

    #[test]
    fn enqueue_starts_the_download_when_idle_and_only_queues_when_busy() {
        let mut queue = Vec::new();
        assert_eq!(enqueue(&mut queue, queued(10), false), QueueEffect::StartFront);
        assert_eq!(enqueue(&mut queue, queued(20), true), QueueEffect::PersistOnly);
        assert_eq!(ids(&queue), vec![10, 20]);
    }

    #[test]
    fn enqueue_rejects_a_duplicate_app_id() {
        let mut queue = vec![queued(10)];
        assert_eq!(enqueue(&mut queue, queued(10), true), QueueEffect::Unchanged);
        assert_eq!(ids(&queue), vec![10], "the same game must not be queued twice");
    }

    /// The original `remove(0)` would have panicked here.
    #[test]
    fn remove_front_on_an_empty_queue_is_a_no_op() {
        let mut queue: Vec<QueuedDownload> = Vec::new();
        assert_eq!(remove_front(&mut queue), QueueEffect::Unchanged);
        assert!(queue.is_empty());
    }

    #[test]
    fn remove_front_drops_the_current_entry_and_starts_the_next() {
        let mut queue = vec![queued(10), queued(20)];
        assert_eq!(remove_front(&mut queue), QueueEffect::StartFront);
        assert_eq!(ids(&queue), vec![20]);
    }

    #[test]
    fn remove_queued_refuses_to_touch_the_current_download() {
        let mut queue = vec![queued(10), queued(20)];
        assert_eq!(remove_queued(&mut queue, 10), QueueEffect::Unchanged);
        assert_eq!(ids(&queue), vec![10, 20]);
        assert_eq!(remove_queued(&mut queue, 20), QueueEffect::PersistOnly);
        assert_eq!(ids(&queue), vec![10]);
        assert_eq!(remove_queued(&mut queue, 999), QueueEffect::Unchanged);
    }

    /// A download that finishes must be removed by App ID, because the queue may have been reordered
    /// while it ran — position 0 is no longer guaranteed to be the game that just completed.
    #[test]
    fn remove_completed_matches_by_app_id_after_a_reorder() {
        let mut queue = vec![queued(10), queued(20), queued(30)];
        assert_eq!(activate(&mut queue, 30, true), QueueEffect::SwitchToFront);
        assert_eq!(ids(&queue), vec![30, 10, 20]);
        // 10 finished even though 30 is now at the front.
        assert_eq!(remove_completed(&mut queue, 10), QueueEffect::StartFront);
        assert_eq!(ids(&queue), vec![30, 20]);
    }

    #[test]
    fn activate_moves_an_entry_to_the_front_and_keeps_the_rest_in_order() {
        let mut queue = vec![queued(10), queued(20), queued(30)];
        assert_eq!(activate(&mut queue, 30, false), QueueEffect::StartFront);
        assert_eq!(ids(&queue), vec![30, 10, 20]);
    }

    #[test]
    fn activating_the_running_front_changes_nothing() {
        let mut queue = vec![queued(10), queued(20)];
        assert_eq!(activate(&mut queue, 10, true), QueueEffect::Unchanged);
        assert_eq!(ids(&queue), vec![10, 20]);
    }

    /// Not running but already at the front: this is the "resume" path, which must still start it.
    #[test]
    fn activating_a_paused_front_restarts_it() {
        let mut queue = vec![queued(10), queued(20)];
        assert_eq!(activate(&mut queue, 10, false), QueueEffect::StartFront);
        assert_eq!(ids(&queue), vec![10, 20]);
    }

    #[test]
    fn activate_ignores_an_unknown_app_id() {
        let mut queue = vec![queued(10)];
        assert_eq!(activate(&mut queue, 999, true), QueueEffect::Unchanged);
        assert_eq!(ids(&queue), vec![10]);
    }

    #[test]
    fn demote_front_rotates_the_queue() {
        let mut queue = vec![queued(10), queued(20), queued(30)];
        assert_eq!(demote_front(&mut queue, true), QueueEffect::SwitchToFront);
        assert_eq!(ids(&queue), vec![20, 30, 10]);
    }

    /// With one entry there is nothing to rotate to; the caller pauses instead.
    #[test]
    fn demote_front_with_a_single_entry_is_a_no_op() {
        let mut queue = vec![queued(10)];
        assert_eq!(demote_front(&mut queue, true), QueueEffect::Unchanged);
        assert_eq!(ids(&queue), vec![10]);
        let mut empty: Vec<QueuedDownload> = Vec::new();
        assert_eq!(demote_front(&mut empty, true), QueueEffect::Unchanged);
    }

    /// Repeated demotion must visit every entry and come back round, never losing or duplicating one.
    #[test]
    fn rotating_through_the_whole_queue_preserves_every_entry() {
        let mut queue = vec![queued(10), queued(20), queued(30)];
        for _ in 0..3 {
            demote_front(&mut queue, true);
        }
        assert_eq!(
            ids(&queue),
            vec![10, 20, 30],
            "a full rotation returns to the start"
        );
    }
}
