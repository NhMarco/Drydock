//! Checks an installed app against its depot manifests without downloading anything.
//!
//! Every chunk is read back and its Adler-32 compared with the manifest. On an SSD that reading
//! scales with parallel requests, so the files are checked by several threads at once. On a hard
//! disk parallel reads only make the heads seek back and forth, so there it stays on one thread
//! unless the user says otherwise (see [`verify_threads`]).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use super::crypto::steam_adler_hash;
use super::download::{DepotData, DepotDownloadError, DownloadProgress, DownloadStage, joined};
use super::manifest::{ChunkEntry, FileEntry};

/// The most threads a verify uses, whatever the setting says.
pub const MAXIMUM_VERIFY_THREADS: u32 = 32;
/// Threads an automatic verify uses at most on an SSD.
const AUTOMATIC_SSD_THREADS: usize = 8;
/// Threads an automatic verify uses at most when Windows cannot say what the drive is (some RAID
/// sets, USB bridges and network shares): enough to help an SSD, few enough not to thrash a disk.
const AUTOMATIC_UNKNOWN_THREADS: usize = 4;
/// How many chunks of a file one unit of work covers. A large file is split so several threads can
/// check it at once, while each unit still reads its part front to back.
const CHUNKS_PER_UNIT: usize = 64;

/// Result of a verify pass.
#[derive(Clone, Debug)]
pub struct VerifyOutcome {
    pub app_id: u32,
    pub total_chunks: u64,
    /// Chunks whose on-disk bytes are missing or fail their Adler-32 (i.e. need re-downloading).
    pub bad_chunks: u64,
    pub bad_files: u64,
}

impl VerifyOutcome {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.bad_chunks == 0 && self.bad_files == 0
    }
}

/// How many threads to verify the install at `install_root` with.
///
/// `setting` is the user's choice, capped at [`MAXIMUM_VERIFY_THREADS`]; `0` means automatic: one
/// thread on a hard disk, otherwise one per core up to eight (four when the drive type is unknown).
#[must_use]
pub fn verify_threads(setting: u32, install_root: &Path) -> usize {
    if setting > 0 {
        return setting.min(MAXIMUM_VERIFY_THREADS) as usize;
    }
    let cores = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get);
    automatic_threads(incurs_seek_penalty(install_root), cores)
}

fn automatic_threads(seek_penalty: Option<bool>, cores: usize) -> usize {
    match seek_penalty {
        Some(true) => 1,
        Some(false) => cores.clamp(1, AUTOMATIC_SSD_THREADS),
        None => cores.clamp(1, AUTOMATIC_UNKNOWN_THREADS),
    }
}

/// Verifies an installed app against its manifests without downloading, counting chunks whose
/// on-disk bytes are missing or fail their Adler-32, on `threads` threads.
pub fn verify(
    data: &DepotData,
    install_root: &Path,
    cancel: &AtomicBool,
    threads: usize,
    mut progress: impl FnMut(DownloadProgress),
) -> Result<VerifyOutcome, DepotDownloadError> {
    let total = data.total_bytes();
    let files: Vec<(&FileEntry, PathBuf)> = data
        .manifests
        .iter()
        .flat_map(|manifest| &manifest.files)
        .filter(|file| !file.is_directory())
        .map(|file| (file, joined(install_root, &file.path)))
        .collect();
    // Every file gets at least one unit, even without chunks: its first unit also checks its size.
    let mut units: Vec<(usize, Range<usize>)> = Vec::new();
    for (index, (file, _)) in files.iter().enumerate() {
        let count = file.chunks.len();
        if count == 0 {
            units.push((index, 0..0));
        }
        for start in (0..count).step_by(CHUNKS_PER_UNIT) {
            units.push((index, start..(start + CHUNKS_PER_UNIT).min(count)));
        }
    }
    let total_chunks = files.iter().map(|(file, _)| file.chunks.len() as u64).sum();

    let pass = Pass {
        files: &files,
        units: &units,
        cancel,
        next: AtomicUsize::new(0),
        checked: AtomicUsize::new(0),
        done: AtomicU64::new(0),
        bad_chunks: AtomicU64::new(0),
        bad_files: AtomicU64::new(0),
        current_file: Mutex::new(String::new()),
    };
    std::thread::scope(|scope| {
        for _ in 0..threads.clamp(1, MAXIMUM_VERIFY_THREADS as usize) {
            scope.spawn(|| pass.check_units());
        }
        loop {
            let name = pass
                .current_file
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            progress(DownloadProgress {
                app_id: data.app_id,
                stage: DownloadStage::Verifying,
                done_bytes: pass.done.load(Ordering::Relaxed).min(total),
                total_bytes: total,
                // A verify only reads: every byte it works through is disk, never network.
                network_bytes: 0,
                disk_bytes: pass.done.load(Ordering::Relaxed),
                current_file: name,
            });
            if pass.checked.load(Ordering::Relaxed) >= units.len() || cancel.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return Err(DepotDownloadError::Cancelled);
    }

    Ok(VerifyOutcome {
        app_id: data.app_id,
        total_chunks,
        bad_chunks: pass.bad_chunks.into_inner(),
        bad_files: pass.bad_files.into_inner(),
    })
}

/// What the threads of one verify share.
struct Pass<'a> {
    files: &'a [(&'a FileEntry, PathBuf)],
    /// `(index into files, range of its chunks)`, in file order.
    units: &'a [(usize, Range<usize>)],
    cancel: &'a AtomicBool,
    next: AtomicUsize,
    /// Units fully checked.
    checked: AtomicUsize,
    done: AtomicU64,
    bad_chunks: AtomicU64,
    bad_files: AtomicU64,
    current_file: Mutex<String>,
}

impl Pass<'_> {
    /// One thread: checks units until none are left or the verify is cancelled.
    fn check_units(&self) {
        // The file this thread read last, with its length, or `None` when it could not be opened.
        let mut open: Option<(usize, Option<(File, u64)>)> = None;
        let mut buffer = Vec::new();
        while !self.cancel.load(Ordering::Relaxed) {
            let Some((index, range)) = self.units.get(self.next.fetch_add(1, Ordering::Relaxed)) else {
                break;
            };
            let (file, path) = &self.files[*index];
            if open.as_ref().is_none_or(|(current, _)| current != index) {
                open = Some((*index, open_for_reading(path)));
            }
            let Some((_, handle)) = open.as_mut() else {
                break;
            };
            if range.start == 0 && handle.as_ref().is_none_or(|(_, length)| *length != file.size) {
                self.bad_files.fetch_add(1, Ordering::Relaxed);
            }
            file.path
                .clone_into(&mut self.current_file.lock().unwrap_or_else(PoisonError::into_inner));
            for chunk in &file.chunks[range.clone()] {
                if self.cancel.load(Ordering::Relaxed) {
                    return;
                }
                let intact = handle
                    .as_mut()
                    .is_some_and(|(handle, length)| chunk_intact(handle, *length, chunk, &mut buffer));
                if !intact {
                    self.bad_chunks.fetch_add(1, Ordering::Relaxed);
                }
                self.done
                    .fetch_add(u64::from(chunk.uncompressed_len), Ordering::Relaxed);
            }
            self.checked.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Opens a regular file and returns it with its length.
fn open_for_reading(path: &Path) -> Option<(File, u64)> {
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    metadata.is_file().then_some((file, metadata.len()))
}

/// Whether the bytes `chunk` describes are in `file` (of `length` bytes) and match its Adler-32.
/// `buffer` is reused between calls.
fn chunk_intact(file: &mut File, length: u64, chunk: &ChunkEntry, buffer: &mut Vec<u8>) -> bool {
    if chunk.offset.saturating_add(u64::from(chunk.uncompressed_len)) > length {
        return false;
    }
    buffer.resize(chunk.uncompressed_len as usize, 0);
    file.seek(SeekFrom::Start(chunk.offset)).is_ok()
        && file.read_exact(buffer).is_ok()
        && steam_adler_hash(buffer) == chunk.crc
}

/// Whether the drive holding `path` is a hard disk, as far as Windows can tell.
#[cfg(windows)]
fn incurs_seek_penalty(path: &Path) -> Option<bool> {
    use std::os::windows::ffi::OsStrExt as _;

    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, GetVolumeNameForVolumeMountPointW,
        GetVolumePathNameW, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_PROPERTY_QUERY,
        StorageDeviceSeekPenaltyProperty,
    };

    // The install root may not exist yet; its nearest existing ancestor is on the same volume.
    let existing = path.ancestors().find(|candidate| candidate.is_dir())?;
    let wide: Vec<u16> = existing.as_os_str().encode_wide().chain([0]).collect();
    let mut mount_point = [0u16; 1024];
    let mut volume = [0u16; 64];
    // SAFETY: `wide` is NUL-terminated and both output buffers are as long as the lengths passed.
    let resolved = unsafe {
        GetVolumePathNameW(wide.as_ptr(), mount_point.as_mut_ptr(), mount_point.len() as u32) != 0
            && GetVolumeNameForVolumeMountPointW(
                mount_point.as_ptr(),
                volume.as_mut_ptr(),
                volume.len() as u32,
            ) != 0
    };
    if !resolved {
        return None;
    }
    // `\\?\Volume{…}\` names the volume's root folder; without the final backslash it names the
    // volume device, which is what the storage query goes to.
    let end = volume.iter().position(|unit| *unit == 0)?;
    if end == 0 || volume[end - 1] != u16::from(b'\\') {
        return None;
    }
    volume[end - 1] = 0;

    // SAFETY: `volume` is NUL-terminated. No access rights are requested, which is all a property
    // query needs and works without elevation.
    let device = unsafe {
        CreateFileW(
            volume.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if device == INVALID_HANDLE_VALUE {
        return None;
    }
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceSeekPenaltyProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    // DEVICE_SEEK_PENALTY_DESCRIPTOR: `u32` version, `u32` size, then the one-byte BOOLEAN. It is
    // read as raw bytes so no byte from the driver ever has to be a valid Rust `bool`.
    let mut descriptor = [0u8; 12];
    let mut returned = 0u32;
    // SAFETY: `device` is an open handle; the input and output pointers and lengths describe
    // `query` and `descriptor`, which outlive this synchronous call.
    let queried = unsafe {
        DeviceIoControl(
            device,
            IOCTL_STORAGE_QUERY_PROPERTY,
            (&raw const query).cast(),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            descriptor.as_mut_ptr().cast(),
            descriptor.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        ) != 0
    };
    // SAFETY: `device` came from CreateFileW above and is closed exactly once.
    unsafe { CloseHandle(device) };
    (queried && returned >= 9).then_some(descriptor[8] != 0)
}

#[cfg(not(windows))]
fn incurs_seek_penalty(_path: &Path) -> Option<bool> {
    // The Windows build is the one that ships; elsewhere the drive type counts as unknown.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::depot::keys::DepotKeys;
    use crate::depot::manifest::DepotManifest;

    fn data_with(files: Vec<FileEntry>) -> DepotData {
        DepotData {
            app_id: 730,
            keys: DepotKeys::default(),
            manifests: vec![DepotManifest {
                depot_id: 1,
                manifest_gid: 1,
                filenames_encrypted: false,
                files,
            }],
            ..DepotData::default()
        }
    }

    /// Writes `path` as `count` chunks of `size` bytes each and describes it; chunk `bad` (if any)
    /// is recorded with a checksum the bytes do not have.
    fn chunked_file(root: &Path, path: &str, count: usize, size: usize, bad: Option<usize>) -> FileEntry {
        let mut bytes = Vec::with_capacity(count * size);
        let mut chunks = Vec::with_capacity(count);
        for index in 0..count {
            let piece: Vec<u8> = (0..size).map(|byte| (byte * 31 + index * 7) as u8).collect();
            let crc = steam_adler_hash(&piece);
            chunks.push(ChunkEntry {
                sha: [0; 20],
                crc: if bad == Some(index) { crc ^ 1 } else { crc },
                offset: (index * size) as u64,
                uncompressed_len: size as u32,
                compressed_len: size as u32,
            });
            bytes.extend_from_slice(&piece);
        }
        std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
        std::fs::write(root.join(path), &bytes).unwrap();
        FileEntry {
            path: path.into(),
            size: bytes.len() as u64,
            flags: 0,
            chunks,
        }
    }

    #[test]
    fn verify_reports_missing_file_as_bad() {
        let dir = tempfile::tempdir().unwrap();
        let data = data_with(vec![FileEntry {
            path: "missing.bin".into(),
            size: 16,
            flags: 0,
            chunks: vec![ChunkEntry {
                sha: [0; 20],
                crc: 123,
                offset: 0,
                uncompressed_len: 16,
                compressed_len: 16,
            }],
        }]);
        let outcome = verify(&data, dir.path(), &AtomicBool::new(false), 1, |_| {}).unwrap();
        assert_eq!(outcome.total_chunks, 1);
        assert_eq!(outcome.bad_chunks, 1);
        assert_eq!(outcome.bad_files, 1);
        assert!(!outcome.is_complete());
    }

    #[test]
    fn verify_accepts_matching_on_disk_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let data = data_with(vec![chunked_file(dir.path(), "ok.bin", 1, 16, None)]);
        let outcome = verify(&data, dir.path(), &AtomicBool::new(false), 1, |_| {}).unwrap();
        assert_eq!(outcome.bad_chunks, 0);
        assert!(outcome.is_complete());
    }

    #[test]
    fn every_thread_count_finds_the_same_damage() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = vec![
            // Large enough to be split across several units.
            chunked_file(dir.path(), "big.bin", 3 * CHUNKS_PER_UNIT + 5, 64, Some(100)),
            chunked_file(dir.path(), "sub/small.bin", 3, 32, None),
            chunked_file(dir.path(), "sub/truncated.bin", 4, 32, None),
            FileEntry {
                path: "empty.txt".into(),
                size: 0,
                flags: 0,
                chunks: Vec::new(),
            },
        ];
        std::fs::write(dir.path().join("empty.txt"), b"").unwrap();
        // Cut the last file short: its last chunk is gone and its size is wrong.
        let truncated = dir.path().join("sub/truncated.bin");
        std::fs::write(&truncated, &std::fs::read(&truncated).unwrap()[..3 * 32]).unwrap();
        files.push(FileEntry {
            path: "absent.bin".into(),
            size: 10,
            flags: 0,
            chunks: files[1].chunks[..1].to_vec(),
        });
        let data = data_with(files);

        for threads in [1, 2, 3, 8, 32] {
            let mut last_done = 0;
            let outcome = verify(&data, dir.path(), &AtomicBool::new(false), threads, |tick| {
                assert!(tick.done_bytes >= last_done, "progress never goes backwards");
                last_done = tick.done_bytes;
            })
            .unwrap();
            assert_eq!(outcome.total_chunks, (3 * CHUNKS_PER_UNIT + 5 + 3 + 4 + 1) as u64);
            assert_eq!(
                outcome.bad_chunks, 3,
                "one corrupt, one cut off, one absent ({threads} threads)"
            );
            assert_eq!(
                outcome.bad_files, 2,
                "the truncated and the absent file ({threads} threads)"
            );
        }
    }

    #[test]
    fn a_cancelled_verify_reports_the_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let data = data_with(vec![chunked_file(dir.path(), "a.bin", 4, 16, None)]);
        let cancel = AtomicBool::new(true);
        assert!(matches!(
            verify(&data, dir.path(), &cancel, 4, |_| {}),
            Err(DepotDownloadError::Cancelled)
        ));
    }

    #[test]
    fn automatic_thread_counts_follow_the_drive() {
        assert_eq!(
            automatic_threads(Some(true), 16),
            1,
            "a hard disk reads one place at a time"
        );
        assert_eq!(automatic_threads(Some(false), 16), AUTOMATIC_SSD_THREADS);
        assert_eq!(automatic_threads(Some(false), 2), 2);
        assert_eq!(automatic_threads(None, 16), AUTOMATIC_UNKNOWN_THREADS);
        assert_eq!(automatic_threads(Some(false), 0), 1);
        // A chosen count is used as is, within the cap.
        let root = Path::new(".");
        assert_eq!(verify_threads(3, root), 3);
        assert_eq!(verify_threads(500, root), MAXIMUM_VERIFY_THREADS as usize);
        assert!(verify_threads(0, root) >= 1);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "depends on the machine's drives"]
    fn the_drive_type_query_does_not_fail_for_a_local_folder() {
        // The answer depends on the machine; the query itself must work without elevation on the
        // system drive, which is always a local volume.
        let temp = std::env::temp_dir();
        assert!(
            incurs_seek_penalty(&temp).is_some(),
            "no answer for {}",
            temp.display()
        );
    }
}
