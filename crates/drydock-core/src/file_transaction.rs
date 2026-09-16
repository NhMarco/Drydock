//! Recoverable file replacement. Live targets are only changed after a complete same-volume copy.
use std::{
    collections::HashSet,
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
};

pub(crate) struct FileTransaction {
    scope: crate::safe_path::WriteRoot,
    backup: Option<tempfile::TempDir>,
    changed: Vec<(PathBuf, Option<PathBuf>)>,
    remembered: HashSet<PathBuf>,
    /// Folders this transaction created, outermost first, so a rollback can remove them again.
    created_folders: Vec<PathBuf>,
    finished: bool,
}

impl FileTransaction {
    /// Keeps the backups in the system temp folder, which suits the small files in Steam's folders.
    pub(crate) fn new(root: &Path) -> io::Result<Self> {
        let scope = crate::safe_path::WriteRoot::new(root)?;
        Self::with_backups(
            scope,
            tempfile::Builder::new().prefix("drydock-recovery-").tempdir()?,
        )
    }

    /// Keeps the backups inside `root`, on the same drive as the files they copy. Game files can be far
    /// larger than the free space left on the system drive.
    pub(crate) fn backed_up_in_root(root: &Path) -> io::Result<Self> {
        let scope = crate::safe_path::WriteRoot::new(root)?;
        Self::with_backups(
            scope,
            tempfile::Builder::new()
                .prefix(".drydock-recovery-")
                .tempdir_in(root)?,
        )
    }

    fn with_backups(scope: crate::safe_path::WriteRoot, backup: tempfile::TempDir) -> io::Result<Self> {
        Ok(Self {
            scope,
            backup: Some(backup),
            changed: Vec::new(),
            remembered: HashSet::new(),
            created_folders: Vec::new(),
            finished: false,
        })
    }

    /// Notes the folders that writing into `folder` is about to create.
    fn remember_new_folders(&mut self, folder: &Path) {
        let first = self.created_folders.len();
        let mut current = folder;
        while current != self.scope.root() && current.starts_with(self.scope.root()) && !current.exists() {
            self.created_folders.push(current.to_owned());
            let Some(parent) = current.parent() else {
                break;
            };
            current = parent;
        }
        self.created_folders[first..].reverse();
    }

    /// Creates `folder` and any missing parents; a rollback removes the ones it created.
    pub(crate) fn create_dir_all(&mut self, folder: &Path) -> io::Result<()> {
        self.remember_new_folders(folder);
        self.scope.create_dir_all(folder)
    }

    fn remember(&mut self, target: &Path) -> io::Result<()> {
        if self.remembered.contains(target) {
            return Ok(());
        }
        let directory = self.backup.as_ref().unwrap().path().to_owned();
        let backup = if target.exists() {
            let path = directory.join(self.changed.len().to_string());
            let mut output = fs::File::create(&path)?;
            // Reading is all a backup needs; a read-only original must still be removable.
            let mut original = self.scope.open_read(target)?;
            io::copy(&mut original, &mut output)?;
            output.set_permissions(original.metadata()?.permissions())?;
            output.sync_all()?;
            Some(path)
        } else {
            None
        };
        // Keep a human-readable recovery map if the process terminates or rollback fails. One JSON line
        // per target is appended, so a transaction over many files does not rewrite the whole map each time.
        let mut line = serde_json::to_vec(&(target, &backup))?;
        line.push(b'\n');
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join("targets.jsonl"))?
            .write_all(&line)?;
        self.changed.push((target.to_owned(), backup));
        self.remembered.insert(target.to_owned());
        Ok(())
    }

    pub(crate) fn replace(&mut self, source: &Path, target: &Path) -> io::Result<()> {
        self.remember(target)?;
        if let Some(parent) = target.parent() {
            self.remember_new_folders(parent);
        }
        self.scope.replace(source, target)
    }

    pub(crate) fn remove(&mut self, target: &Path) -> io::Result<()> {
        self.remember(target)?;
        if target.exists() {
            self.scope.remove(target)?;
        }
        Ok(())
    }

    pub(crate) fn commit(mut self) {
        self.finished = true;
    }

    pub(crate) fn rollback(&mut self) -> io::Result<()> {
        let mut failures = Vec::new();
        for (target, backup) in self.changed.iter().rev() {
            let result = match backup {
                Some(source) => self.scope.replace(source, target),
                None => self.scope.remove(target).or_else(|error| {
                    if error.kind() == io::ErrorKind::NotFound {
                        Ok(())
                    } else {
                        Err(error)
                    }
                }),
            };
            if let Err(error) = result {
                failures.push(format!("{}: {error}", target.display()));
            }
        }
        // Innermost first. Only an empty folder goes: anything still inside was not ours to remove.
        for folder in self.created_folders.iter().rev() {
            let _ = self.scope.remove_empty_dir(folder);
        }
        self.finished = true;
        if failures.is_empty() {
            return Ok(());
        }
        let recovery = self.backup.take().unwrap().keep();
        Err(io::Error::other(format!(
            "Rollback incomplete; originals retained at {}: {}",
            recovery.display(),
            failures.join("; ")
        )))
    }
}

impl Drop for FileTransaction {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.rollback();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_replacement_preserves_originals() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("old");
        fs::write(&target, "original").unwrap();
        let mut tx = FileTransaction::new(dir.path()).unwrap();
        assert!(tx.replace(&dir.path().join("missing"), &target).is_err());
        tx.rollback().unwrap();
        assert_eq!(fs::read_to_string(target).unwrap(), "original");
    }

    #[test]
    fn rollback_restores_replaced_files_and_removes_new_files() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        let new = dir.path().join("new");
        let source = dir.path().join("source");
        fs::write(&old, "original").unwrap();
        fs::write(&source, "replacement").unwrap();
        let mut tx = FileTransaction::new(dir.path()).unwrap();
        tx.replace(&source, &old).unwrap();
        tx.replace(&source, &new).unwrap();
        assert!(tx.replace(&dir.path().join("missing"), &old).is_err());
        tx.rollback().unwrap();
        assert_eq!(fs::read_to_string(&old).unwrap(), "original");
        assert!(!new.exists());
    }

    #[test]
    fn failed_rollback_keeps_recovery_map_and_original() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        let source = dir.path().join("source");
        fs::write(&old, "original").unwrap();
        fs::write(&source, "replacement").unwrap();
        let mut tx = FileTransaction::new(dir.path()).unwrap();
        tx.replace(&source, &old).unwrap();
        let recovery = tx.backup.as_ref().unwrap().path().to_owned();
        fs::remove_file(&old).unwrap();
        fs::create_dir(&old).unwrap();
        assert!(tx.rollback().is_err());
        drop(tx);
        assert_eq!(fs::read_to_string(recovery.join("0")).unwrap(), "original");
        let map = fs::read_to_string(recovery.join("targets.jsonl")).unwrap();
        assert_eq!(map.lines().count(), 1);
        fs::remove_dir_all(recovery).unwrap();
    }

    #[test]
    fn read_only_files_can_be_removed_and_restored() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("service.dll");
        fs::write(&target, "original").unwrap();
        let mut permissions = fs::metadata(&target).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&target, permissions).unwrap();
        let mut tx = FileTransaction::new(dir.path()).unwrap();
        tx.remove(&target).unwrap();
        assert!(!target.exists());
        tx.rollback().unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "original");
        assert!(fs::metadata(&target).unwrap().permissions().readonly());
    }
}
