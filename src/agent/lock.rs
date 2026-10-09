use anyhow::{Context, Result, bail};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Exclusive lock mechanism for remote worktrees using atomic file creation (`.rbuild.lock`).
///
/// Current limitation: Lock release relies on the `Drop` trait implementation removing the
/// lock file. If the process is terminated abruptly (e.g. SIGKILL, panic without unwinding,
/// system crash), the `.rbuild.lock` file will persist on disk ("stale lock"). In such cases,
/// subsequent acquire attempts will fail until the lock file is manually cleared.
/// Process liveness checks (e.g. inspecting recorded PID) or OS-level advisory locks (`flock`)
/// are deferred to future units.
pub struct WorktreeLock {
    lock_file_path: PathBuf,
}

impl WorktreeLock {
    /// Attempts to acquire an exclusive lock on the given worktree directory.
    ///
    /// Creates `.rbuild.lock` using `OpenOptions::create_new(true)`.
    /// Note: Stale lock files from crashed processes must be manually removed.
    pub fn acquire(worktree_dir: &Path) -> Result<Self> {
        fs::create_dir_all(worktree_dir)
            .with_context(|| format!("Creating worktree dir: {}", worktree_dir.display()))?;

        let lock_file_path = worktree_dir.join(".rbuild.lock");

        // Try exclusive file creation
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_file_path)
        {
            Ok(mut file) => {
                let pid = std::process::id();
                let _ = writeln!(file, "pid={}", pid);
                Ok(Self { lock_file_path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                bail!(
                    "Worktree is already locked by another process: {}",
                    lock_file_path.display()
                );
            }
            Err(e) => {
                bail!(
                    "Failed to acquire lock at {}: {}",
                    lock_file_path.display(),
                    e
                );
            }
        }
    }
}

impl Drop for WorktreeLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_file_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lock_acquire_and_release() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("worktree");

        {
            let lock = WorktreeLock::acquire(&wt).unwrap();
            assert!(wt.join(".rbuild.lock").exists());

            // Second acquisition must fail while held
            let lock2 = WorktreeLock::acquire(&wt);
            assert!(lock2.is_err());
            drop(lock);
        }

        // After drop, lock file should be gone and acquisition succeed
        let lock3 = WorktreeLock::acquire(&wt);
        assert!(lock3.is_ok());
    }
}
