//! Path-segment validation shared by everything that writes remote-sourced files to disk.
//!
//! Depot manifests, fix archives and emulator skeleton ZIPs all carry relative paths that Drydock
//! joins onto a target folder. Filtering only `..` is not enough on Windows: [`std::path::PathBuf`]
//! treats a bare drive specifier as a path prefix, so pushing the segment `C:` **replaces the whole
//! accumulated path** instead of appending to it —
//!
//! ```text
//! Path::new(r"D:\Games\App").join("C:")  ==  Path::new("C:")
//! ```
//!
//! which turns a manifest entry like `C:/Windows/System32/evil.dll` into a write outside the
//! install root. Verbatim/UNC prefixes (`\\?\`, `\\server\share`) behave the same way. Callers
//! therefore push segments only after [`is_safe_path_segment`] accepts them.

/// Whether `segment` may be appended to a target path.
///
/// Rejects empty segments, `.`/`..`, anything containing a path separator or a colon (which covers
/// drive letters like `C:` and NTFS alternate data streams like `file.txt:hidden`), and reserved
/// characters that Windows would otherwise interpret. Rejecting rather than sanitising is
/// deliberate: a segment we do not fully understand must not silently become a different one.
#[must_use]
pub fn is_safe_path_segment(segment: &str) -> bool {
    if segment.is_empty() || segment == "." || segment == ".." {
        return false;
    }
    if segment.contains(['/', '\\', ':']) {
        return false;
    }
    // A trailing dot or space is stripped by the Win32 path layer, so `foo.` and `foo` would land on
    // the same file — and a segment of only dots is a traversal spelling we have not seen.
    if segment.ends_with('.') || segment.ends_with(' ') || segment.chars().all(|c| c == '.') {
        return false;
    }
    true
}

/// Splits a relative path on both separators and keeps only the segments that pass
/// [`is_safe_path_segment`], so the result can never escape the folder it is joined onto.
pub fn safe_segments(relative: &str) -> impl Iterator<Item = &str> {
    relative
        .split(['/', '\\'])
        .filter(|segment| is_safe_path_segment(segment))
}

/// Joins a remote-sourced relative path onto `root`, dropping any segment that could escape it.
#[must_use]
pub fn join_within(root: &std::path::Path, relative: &str) -> std::path::PathBuf {
    let mut path = root.to_path_buf();
    for segment in safe_segments(relative) {
        path.push(segment);
    }
    path
}

/// A reservation hint for reading an archive entry whose declared size we do not trust.
///
/// A ZIP entry's uncompressed size comes straight from the archive header, so passing it to
/// `Vec::with_capacity` lets a crafted (or simply corrupt) archive demand an arbitrary allocation up
/// front — a multi-gigabyte reservation for a few bytes of actual data. Clamping the *hint* keeps the
/// fast path (exact reservation for ordinary files) while making the pathological case grow the
/// buffer normally instead of allocating on trust.
#[must_use]
pub fn capacity_hint(declared_size: u64) -> usize {
    /// 64 MiB covers every entry Drydock legitimately reads (the largest is a ~20 MB steamclient DLL).
    const MAXIMUM_HINT: u64 = 64 * 1024 * 1024;
    usize::try_from(declared_size.min(MAXIMUM_HINT)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn accepts_ordinary_segments() {
        assert!(is_safe_path_segment("game.exe"));
        assert!(is_safe_path_segment("Bin64"));
        assert!(is_safe_path_segment("steam_settings"));
        assert!(
            is_safe_path_segment("..hidden"),
            "a leading dot pair is a real name"
        );
    }

    #[test]
    fn rejects_traversal_and_separators() {
        assert!(!is_safe_path_segment(""));
        assert!(!is_safe_path_segment("."));
        assert!(!is_safe_path_segment(".."));
        assert!(!is_safe_path_segment("..."));
        assert!(!is_safe_path_segment("a/b"));
        assert!(!is_safe_path_segment("a\\b"));
    }

    /// The reason this module exists: `PathBuf::push("C:")` discards everything pushed before it.
    #[test]
    fn rejects_windows_drive_specifiers_and_streams() {
        assert!(!is_safe_path_segment("C:"));
        assert!(!is_safe_path_segment("c:"));
        assert!(!is_safe_path_segment("file.txt:stream"));
    }

    #[test]
    fn rejects_trailing_dot_or_space_aliases() {
        assert!(!is_safe_path_segment("game.exe."));
        assert!(!is_safe_path_segment("game.exe "));
    }

    #[test]
    fn join_within_never_leaves_the_root() {
        let root = Path::new("D:/Games/App");
        assert_eq!(
            join_within(root, "bin/game.exe"),
            root.join("bin").join("game.exe")
        );
        assert_eq!(
            join_within(root, "sub/../../etc/passwd"),
            root.join("sub").join("etc").join("passwd")
        );
        // Without the colon check this collapses to `C:\Windows\System32\evil.dll`.
        let escaped = join_within(root, "C:/Windows/System32/evil.dll");
        assert!(
            escaped.starts_with(root),
            "joined path escaped the root: {}",
            escaped.display()
        );
        assert_eq!(escaped, root.join("Windows").join("System32").join("evil.dll"));
    }

    #[test]
    fn capacity_hint_clamps_untrusted_sizes() {
        assert_eq!(capacity_hint(0), 0);
        assert_eq!(capacity_hint(1024), 1024);
        // A header claiming 10 GB must not turn into a 10 GB reservation.
        assert_eq!(capacity_hint(10 * 1024 * 1024 * 1024), 64 * 1024 * 1024);
        assert_eq!(capacity_hint(u64::MAX), 64 * 1024 * 1024);
    }

    #[test]
    fn join_within_ignores_unc_and_verbatim_prefixes() {
        let root = Path::new("D:/Games/App");
        for hostile in [
            r"\\?\C:\Windows\x.dll",
            r"\\server\share\x.dll",
            "//server/share/x.dll",
        ] {
            let joined = join_within(root, hostile);
            assert!(
                joined.starts_with(root),
                "{hostile} escaped to {}",
                joined.display()
            );
        }
    }
}
