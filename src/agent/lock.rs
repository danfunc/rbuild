use anyhow::{Context, Result, bail};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const LOCK_FILE_NAME: &str = ".rbuild.lock";
const RECOVERY_FILE_NAME: &str = ".rbuild.lock.reap";

/// Exclusive lock mechanism for remote worktrees using atomic file creation (`.rbuild.lock`).
///
/// Release relies on the `Drop` implementation removing the lock file. If the process is
/// killed abruptly (e.g. SIGKILL, system crash), the file persists ("stale lock").
/// A stale lock is recovered only when the recorded PID is demonstrably absent:
/// - Linux: `/proc/<pid>` does not exist.
/// - Other Unix: `kill -0` reports "No such process". Permission errors (EPERM) and any
///   other unknown result are treated as "alive", so the lock is kept.
///
/// Recovery is serialized through `.rbuild.lock.reap`, and the staleness check is repeated
/// while holding it, so two concurrent acquirers cannot both reap and then both recreate
/// the lock. If a process dies while holding `.rbuild.lock.reap`, that file must be removed
/// manually once no rbuild process is using the worktree.
pub struct WorktreeLock {
    lock_file_path: PathBuf,
    pid: u32,
}

impl WorktreeLock {
    /// Attempts to acquire an exclusive lock on the given worktree directory.
    ///
    /// Creates `.rbuild.lock` using `OpenOptions::create_new(true)`.
    /// Existing locks are recovered only when their recorded process is demonstrably absent.
    pub fn acquire(worktree_dir: &Path) -> Result<Self> {
        fs::create_dir_all(worktree_dir)
            .with_context(|| format!("Creating worktree dir: {}", worktree_dir.display()))?;

        let lock_file_path = worktree_dir.join(LOCK_FILE_NAME);
        if let Some(lock) = try_create(&lock_file_path)? {
            return Ok(lock);
        }

        if !stale_lock(&lock_file_path) {
            bail!(
                "Worktree is already locked by another process: {}",
                lock_file_path.display()
            );
        }

        let _recovery = RecoveryGuard::acquire(worktree_dir)?;
        // Re-check under the recovery guard: another acquirer may have reaped the lock and
        // created a live one since the first check.
        if stale_lock(&lock_file_path) {
            match fs::remove_file(&lock_file_path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => bail!(
                    "Stale lock could not be removed at {}: {}",
                    lock_file_path.display(),
                    e
                ),
            }
        }

        match try_create(&lock_file_path)? {
            Some(lock) => Ok(lock),
            None => bail!(
                "Worktree is already locked by another process: {}",
                lock_file_path.display()
            ),
        }
    }
}

/// Creates the lock file exclusively. Returns `Ok(None)` when it already exists.
fn try_create(lock_file_path: &Path) -> Result<Option<WorktreeLock>> {
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock_file_path)
    {
        Ok(mut file) => {
            let pid = std::process::id();
            if let Err(e) = writeln!(file, "pid={}", pid) {
                let _ = fs::remove_file(lock_file_path);
                bail!(
                    "Failed to record owner in lock at {}: {}",
                    lock_file_path.display(),
                    e
                );
            }
            Ok(Some(WorktreeLock {
                lock_file_path: lock_file_path.to_path_buf(),
                pid,
            }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
        Err(e) => bail!(
            "Failed to acquire lock at {}: {}",
            lock_file_path.display(),
            e
        ),
    }
}

/// Exclusive guard serializing stale-lock recovery within one worktree.
struct RecoveryGuard {
    path: PathBuf,
}

impl RecoveryGuard {
    fn acquire(worktree_dir: &Path) -> Result<Self> {
        let path = worktree_dir.join(RECOVERY_FILE_NAME);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let _ = writeln!(file, "pid={}", std::process::id());
                Ok(Self { path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => bail!(
                "Stale lock recovery is already in progress at {}. If no rbuild process is using this worktree, remove that file and retry.",
                path.display()
            ),
            Err(e) => bail!(
                "Failed to start stale lock recovery at {}: {}",
                path.display(),
                e
            ),
        }
    }
}

impl Drop for RecoveryGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn parse_pid(text: &str) -> Option<u32> {
    text.trim().strip_prefix("pid=")?.trim().parse::<u32>().ok()
}

fn stale_lock(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let Some(pid) = parse_pid(&text) else {
        return false;
    };
    if pid == 0 {
        return false;
    }
    process_is_absent(pid)
}

/// Returns true only when the process is proven to be absent.
fn process_is_absent(pid: u32) -> bool {
    // Process IDs are pid_t values; anything above i32::MAX cannot name a live process.
    if pid > i32::MAX as u32 {
        return true;
    }
    if Path::new("/proc").is_dir() {
        return !Path::new("/proc").join(pid.to_string()).exists();
    }
    // `kill -0` fails with EPERM for live processes owned by other users, so only the
    // "No such process" diagnostic counts as absence. LC_ALL=C keeps the text stable.
    match Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(out) => {
            !out.status.success()
                && String::from_utf8_lossy(&out.stderr)
                    .to_ascii_lowercase()
                    .contains("no such process")
        }
        Err(_) => false,
    }
}

impl Drop for WorktreeLock {
    fn drop(&mut self) {
        // Remove the file only if it still records this process as owner.
        let owned = fs::read_to_string(&self.lock_file_path)
            .ok()
            .and_then(|text| parse_pid(&text))
            == Some(self.pid);
        if owned {
            let _ = fs::remove_file(&self.lock_file_path);
        }
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

    #[test]
    fn test_lock_recovers_dead_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("worktree");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".rbuild.lock"), "pid=4294967294\n").unwrap();
        let lock = WorktreeLock::acquire(&wt).unwrap();
        assert!(lock.lock_file_path.exists());
        assert!(
            fs::read_to_string(wt.join(".rbuild.lock"))
                .unwrap()
                .contains(&std::process::id().to_string())
        );
        // The recovery guard must not outlive acquisition
        assert!(!wt.join(".rbuild.lock.reap").exists());
    }

    #[test]
    fn test_lock_held_by_live_pid_is_not_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("worktree");
        fs::create_dir_all(&wt).unwrap();
        // Our own PID is certainly alive
        fs::write(
            wt.join(".rbuild.lock"),
            format!("pid={}\n", std::process::id()),
        )
        .unwrap();
        assert!(WorktreeLock::acquire(&wt).is_err());
        assert!(wt.join(".rbuild.lock").exists());
    }

    #[test]
    fn test_lock_owned_by_other_user_pid_is_not_recovered() {
        // PID 1 always exists; non-root callers get EPERM from `kill -0`, which must count as alive.
        assert!(!stale_lock_for_pid(1));
    }

    #[test]
    fn test_unparseable_lock_is_not_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("worktree");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".rbuild.lock"), "garbage").unwrap();
        assert!(WorktreeLock::acquire(&wt).is_err());
        assert!(wt.join(".rbuild.lock").exists());
    }

    #[test]
    fn test_recovery_in_progress_blocks_reaping() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("worktree");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".rbuild.lock"), "pid=4294967294\n").unwrap();
        fs::write(wt.join(".rbuild.lock.reap"), "pid=1\n").unwrap();
        let err = WorktreeLock::acquire(&wt).err().expect("must fail");
        assert!(err.to_string().contains("already in progress"));
        assert!(wt.join(".rbuild.lock").exists());
    }

    fn stale_lock_for_pid(pid: u32) -> bool {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".rbuild.lock");
        fs::write(&path, format!("pid={pid}\n")).unwrap();
        stale_lock(&path)
    }
}
