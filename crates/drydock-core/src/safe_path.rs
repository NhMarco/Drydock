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

/// Whether a segment of remote metadata that names a whole folder or launch path — Steam's
/// `installdir`, a launch executable — is one Windows can use as written.
///
/// Stricter than [`is_safe_path_segment`]: it also refuses characters Windows reserves, control
/// characters, and device names such as `CON` or `aux.txt`, which Windows may open as a device
/// instead of a file. Depot and archive entries are deliberately not held to this: Linux depots can
/// use those names, and the joiners drop a refused segment, which would move the file elsewhere.
#[must_use]
pub fn is_portable_path_segment(segment: &str) -> bool {
    if !is_safe_path_segment(segment)
        || segment.contains(['<', '>', '"', '|', '?', '*'])
        || segment.chars().any(char::is_control)
    {
        return false;
    }
    let stem = segment
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    }) {
        return false;
    }
    true
}

/// Validate an entire relative path from remote metadata without silently rewriting it.
pub fn relative_path(value: &str) -> Option<std::path::PathBuf> {
    let segments: Vec<_> = value.split(['/', '\\']).collect();
    if segments.iter().any(|segment| !is_portable_path_segment(segment)) {
        return None;
    }
    Some(segments.iter().collect())
}

/// Rejects an existing link — a symlink, junction or mount point — at `target` or at any folder
/// between it and `root`. Missing components are allowed so callers can create them afterwards.
///
/// `root` itself and everything above it are not checked: they were chosen by the user or the app,
/// and a Steam library moved with a junction, or Linux's symlinked `~/.steam/steam`, is an ordinary
/// setup.
pub fn ensure_no_links(root: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    let relative = target
        .strip_prefix(root)
        .map_err(|_| outside_root(root, target))?;
    refuse_links_below(root, relative)
}

fn refuse_links_below(root: &std::path::Path, relative: &std::path::Path) -> std::io::Result<()> {
    let mut path = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(outside_root(root, &root.join(relative)));
        };
        path.push(name);
        match std::fs::symlink_metadata(&path) {
            // On Windows this covers junctions and mount points (name-surrogate reparse points), but
            // not reparse points that are ordinary files or folders, such as cloud-file placeholders.
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("Refusing linked write path: {}", path.display()),
                ));
            }
            Ok(_) => {}
            // Nothing can exist below a folder that does not.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn outside_root(root: &std::path::Path, target: &std::path::Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("{} is not inside {}", target.display(), root.display()),
    )
}

/// A folder that writes are confined to.
///
/// Files are opened through a pinned `cap_std` directory handle, which refuses any link that leads
/// outside the folder. Creating folders, renaming and removing additionally refuse existing links
/// below the folder, because on Windows those go through plain paths (see the note on the Windows
/// fallbacks at the end of the impl). The folder itself may sit behind a link.
pub(crate) struct WriteRoot {
    root: std::path::PathBuf,
    dir: cap_std::fs::Dir,
    /// The folder's final path (`\\?\C:\…` or `\\?\UNC\server\share\…`), which the Windows fallbacks
    /// build on: like the handle, it no longer depends on a link in front of the folder.
    #[cfg(windows)]
    resolved: std::path::PathBuf,
}

impl WriteRoot {
    pub(crate) fn new(root: &std::path::Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(root)?;
        Ok(Self {
            root: root.to_owned(),
            dir: cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())?,
            // A filesystem that cannot report a final path keeps the path as given.
            #[cfg(windows)]
            resolved: std::fs::canonicalize(root).unwrap_or_else(|_| root.to_owned()),
        })
    }

    pub(crate) fn root(&self) -> &std::path::Path {
        &self.root
    }

    fn relative<'a>(&self, target: &'a std::path::Path) -> std::io::Result<&'a std::path::Path> {
        target
            .strip_prefix(&self.root)
            .map_err(|_| outside_root(&self.root, target))
    }

    fn relative_unlinked<'a>(&self, target: &'a std::path::Path) -> std::io::Result<&'a std::path::Path> {
        let relative = self.relative(target)?;
        refuse_links_below(&self.root, relative)?;
        Ok(relative)
    }

    pub(crate) fn create_dir_all(&self, target: &std::path::Path) -> std::io::Result<()> {
        self.create_dir_all_at(self.relative_unlinked(target)?)
    }

    /// Opens an existing file for reading only.
    pub(crate) fn open_read(&self, target: &std::path::Path) -> std::io::Result<std::fs::File> {
        self.dir
            .open(self.relative(target)?)
            .map(cap_std::fs::File::into_std)
    }

    /// Opens a file for reading and writing, creating it when `create` is set. Its folder must exist.
    pub(crate) fn open(&self, target: &std::path::Path, create: bool) -> std::io::Result<std::fs::File> {
        self.dir
            .open_with(
                self.relative(target)?,
                cap_std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(create),
            )
            .map(cap_std::fs::File::into_std)
    }

    pub(crate) fn create(&self, target: &std::path::Path) -> std::io::Result<std::fs::File> {
        let file = self.open(target, true)?;
        file.set_len(0)?;
        Ok(file)
    }

    pub(crate) fn replace(&self, source: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
        let relative = self.relative_unlinked(target)?;
        if let Some(parent) = relative.parent() {
            self.create_dir_all_at(parent)?;
        }
        let temporary = format!(".drydock-{:032x}.tmp", rand::random::<u128>());
        let temporary = std::path::Path::new(&temporary);
        let result = (|| {
            let mut output = self.dir.open_with(
                temporary,
                cap_std::fs::OpenOptions::new().write(true).create_new(true),
            )?;
            std::io::copy(&mut std::fs::File::open(source)?, &mut output)?;
            // Preserve executable/read-only bits when replacing an existing file or restoring a backup.
            let permissions = std::fs::metadata(target)
                .or_else(|_| std::fs::metadata(source))?
                .permissions();
            output.set_permissions(cap_std::fs::Permissions::from_std(permissions))?;
            output.sync_all()?;
            drop(output);
            self.rename_at(temporary, relative)
        })();
        if result.is_err() {
            let _ = self.remove_file_at(temporary);
        }
        result
    }

    pub(crate) fn remove(&self, target: &std::path::Path) -> std::io::Result<()> {
        self.remove_file_at(self.relative_unlinked(target)?)
    }

    /// Removes `folder` below the root if it is empty; a folder with anything left inside is an error.
    pub(crate) fn remove_empty_dir(&self, folder: &std::path::Path) -> std::io::Result<()> {
        let relative = self.relative_unlinked(folder)?;
        if relative.as_os_str().is_empty() {
            return Err(outside_root(&self.root, folder));
        }
        self.remove_dir_at(relative)
    }

    // cap-primitives performs these three through a path string it derives from the directory handle,
    // and on Windows that string turns a network share (`\\?\UNC\server\share`) into a relative path.
    // Windows therefore uses the folder's final path, after the link check; other platforms keep the
    // capability.
    #[cfg(windows)]
    fn resolved(&self, relative: &std::path::Path) -> std::path::PathBuf {
        // Pushed component by component: a verbatim `\\?\` path does not treat `/` as a separator.
        let mut path = self.resolved.clone();
        path.extend(relative.components());
        path
    }

    #[cfg(windows)]
    fn create_dir_all_at(&self, relative: &std::path::Path) -> std::io::Result<()> {
        std::fs::create_dir_all(self.resolved(relative))
    }

    #[cfg(not(windows))]
    fn create_dir_all_at(&self, relative: &std::path::Path) -> std::io::Result<()> {
        self.dir.create_dir_all(relative)
    }

    #[cfg(windows)]
    fn rename_at(&self, from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
        std::fs::rename(self.resolved(from), self.resolved(to))
    }

    #[cfg(not(windows))]
    fn rename_at(&self, from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
        self.dir.rename(from, &self.dir, to)
    }

    #[cfg(windows)]
    fn remove_file_at(&self, relative: &std::path::Path) -> std::io::Result<()> {
        std::fs::remove_file(self.resolved(relative))
    }

    #[cfg(not(windows))]
    fn remove_file_at(&self, relative: &std::path::Path) -> std::io::Result<()> {
        self.dir.remove_file(relative)
    }

    #[cfg(windows)]
    fn remove_dir_at(&self, relative: &std::path::Path) -> std::io::Result<()> {
        std::fs::remove_dir(self.resolved(relative))
    }

    #[cfg(not(windows))]
    fn remove_dir_at(&self, relative: &std::path::Path) -> std::io::Result<()> {
        self.dir.remove_dir(relative)
    }
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

/// Creates `link` as a link to the folder `target`: a junction on Windows, which unlike a symlink
/// needs no privilege, so the tests also run on an ordinary account.
#[cfg(test)]
pub(crate) fn link_folder(target: &std::path::Path, link: &std::path::Path) {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
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

    /// A device name or a character Windows reserves is an ordinary file name in a depot (and Windows
    /// 11 accepts the device names), so joining must keep it: dropping the segment moves the file.
    #[test]
    fn joining_keeps_names_that_only_remote_metadata_refuses() {
        let root = Path::new("D:/Games/App");
        assert_eq!(
            join_within(root, "Data/con/level.pak"),
            root.join("Data").join("con").join("level.pak")
        );
        assert_eq!(
            join_within(root, "Content/Audio/aux.bank"),
            root.join("Content").join("Audio").join("aux.bank")
        );
        for name in ["con", "aux.bank", "COM1.ogg", "what?.txt", "a\u{1}b"] {
            assert!(is_safe_path_segment(name), "{name}");
            assert!(!is_portable_path_segment(name), "{name}");
        }
    }

    #[test]
    fn a_linked_root_is_accepted_but_a_link_below_it_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        let outside = dir.path().join("outside");
        std::fs::create_dir(&real).unwrap();
        std::fs::create_dir(&outside).unwrap();
        // A Steam library moved with a junction, or Linux's symlinked `~/.steam/steam`.
        let linked = dir.path().join("linked");
        link_folder(&real, &linked);
        let scope = WriteRoot::new(&linked).unwrap();
        scope.create_dir_all(&linked.join("bin")).unwrap();
        drop(scope.create(&linked.join("bin").join("game.exe")).unwrap());
        assert!(real.join("bin").join("game.exe").is_file());

        link_folder(&outside, &real.join("escape"));
        let escape = linked.join("escape");
        assert!(ensure_no_links(&linked, &escape.join("file")).is_err());
        assert!(scope.create_dir_all(&escape.join("sub")).is_err());
        assert!(scope.open(&escape.join("file"), true).is_err());
        assert!(
            std::fs::read_dir(&outside).unwrap().next().is_none(),
            "nothing may be written through the link"
        );
    }

    /// cap-primitives turns a share's final path into a relative one, so the Windows fallbacks have to
    /// keep folder creation, replacement and removal working there. Uses the local admin share.
    #[cfg(windows)]
    #[test]
    fn write_root_works_on_a_network_share() {
        let local = tempfile::tempdir().unwrap();
        let absolute = std::path::absolute(local.path()).unwrap();
        let Some((drive, rest)) = absolute.to_str().and_then(|path| path.split_once(":\\")) else {
            return;
        };
        let share = std::path::PathBuf::from(format!(r"\\localhost\{drive}$\{rest}"));
        if !share.is_dir() {
            return; // no administrative share on this machine
        }
        let scope = WriteRoot::new(&share).unwrap();
        let folder = share.join("depot").join("bin");
        scope.create_dir_all(&folder).unwrap();
        drop(scope.create(&folder.join("created.bin")).unwrap());
        let source = local.path().join("source");
        std::fs::write(&source, b"new").unwrap();
        let target = folder.join("game.exe");
        scope.replace(&source, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        scope.remove(&target).unwrap();
        assert!(!target.exists());
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
