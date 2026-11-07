//! One install at a time on a machine, whoever starts it (BOOT3 review, SPEC §24.6): `ptyro
//! install`, `ptyro setup` (the CLI, and the TUI's splits that run it) and the daemon's
//! `SetupRun` (the web) each hold `<state>/setup.lock` while they install, so two package
//! managers never fight over their locks. The holder says who it is in `<state>/setup.who` (a
//! file of its own: Windows' locks keep others from reading a locked file), so a second run says
//! where the first came from. The lock is the operating system's (`File::try_lock`: `flock` on
//! Unix, `LockFileEx` on Windows), so it is let go however the holder ends, a crash included.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The lock files this process holds. Asked first, so this process never probes its own lock:
/// where `flock` is emulated with POSIX locks (NFS), those belong to the process, and a probe's
/// unlock (or its file closing) would let go of the real one.
static HELD: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

fn held_here(path: &Path) -> bool {
    HELD.lock().map(|h| h.contains(path)).unwrap_or(false)
}

/// Holds the machine's setup lock until dropped.
#[derive(Debug)]
pub struct Lock {
    /// `None` when the filesystem can't lock at all: installs go ahead unguarded, as before.
    _file: Option<File>,
    who: PathBuf,
    /// The lock file, in [`HELD`] while held.
    path: PathBuf,
}

impl Lock {
    /// Take the lock in `state` for `who` ("`ptyro install` in a terminal"), or say who has it.
    pub fn take(state: &Path, who: &str) -> Result<Self, String> {
        let _ = std::fs::create_dir_all(state);
        let path = state.join("setup.lock");
        let who_path = state.join("setup.who");
        let file = match OpenOptions::new().create(true).write(true).truncate(false).open(&path) {
            Ok(f) => f,
            // Nowhere to keep it (a read-only state folder): don't block the install over it.
            Err(_) => return Ok(Self { _file: None, who: who_path, path }),
        };
        if held_here(&path) {
            return Err(busy(&holder(&who_path)));
        }
        // A probe (`held_by`) holds it for an instant: tried again for a moment before refusing,
        // so asking who holds it never refuses a run starting then.
        let until = std::time::Instant::now() + PATIENCE;
        loop {
            match file.try_lock() {
                Ok(()) => {
                    let _ = std::fs::write(
                        &who_path,
                        format!("{who} (process {})", std::process::id()),
                    );
                    // A version check going beside this won't write over what it installs
                    // (BOOT5).
                    if let Ok(mut h) = HELD.lock() {
                        h.insert(path.clone());
                    }
                    crate::updates::started(state);
                    return Ok(Self { _file: Some(file), who: who_path, path });
                }
                Err(TryLockError::WouldBlock) if std::time::Instant::now() < until => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(TryLockError::WouldBlock) => return Err(busy(&holder(&who_path))),
                Err(TryLockError::Error(_)) => {
                    return Ok(Self { _file: None, who: who_path, path });
                }
            }
        }
    }

    /// Who holds the lock in `state` now ("`ptyro install` (process 4242)"); `None` when it is
    /// free. For a checklist to say so before anyone ticks anything. Only the operating system's
    /// lock decides: a non-blocking probe (held for an instant; [`Lock::take`] waits that out).
    /// The probe's lock is shared, so two probes at once never take each other for a holder (an
    /// exclusive one did: a checklist then said setup was running when nothing was).
    /// Held, `setup.who` names who; free, a `setup.who` left by a holder that ended without
    /// tidying (Ctrl-C, a closed window, power lost) is removed, whatever process now has its id.
    pub fn held_by(state: &Path) -> Option<String> {
        let path = state.join("setup.lock");
        // This process's own: said without probing it (see [`HELD`]).
        if held_here(&path) {
            return Some(holder(&state.join("setup.who")));
        }
        let file = OpenOptions::new().write(true).open(&path).ok()?;
        match file.try_lock_shared() {
            Ok(()) => {
                // Free: what a gone holder left goes while this probe holds the lock, so a new
                // holder's note is never removed.
                let _ = std::fs::remove_file(state.join("setup.who"));
                let _ = file.unlock();
                None
            }
            Err(TryLockError::WouldBlock) => Some(holder(&state.join("setup.who"))),
            Err(TryLockError::Error(_)) => None,
        }
    }
}

/// How long [`Lock::take`] tries again while the lock is held (a probe's instant, never a run).
const PATIENCE: std::time::Duration = std::time::Duration::from_millis(400);

/// What the holder wrote of itself. (A new holder writes it just after it locks, so for an
/// instant a contender may read the one before, or nothing: cosmetic.)
fn holder(who: &Path) -> String {
    std::fs::read_to_string(who)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "another ptyro".to_owned())
}

/// The refusal, as every client says it.
pub fn busy(holder: &str) -> String {
    format!("setup is already running (from {holder}): wait for it to finish, then try again")
}

impl Drop for Lock {
    fn drop(&mut self) {
        if self._file.is_some() {
            let _ = std::fs::remove_file(&self.who);
            if let Ok(mut h) = HELD.lock() {
                h.remove(&self.path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_holder_is_refused_and_told_who_then_free_again() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Lock::held_by(dir.path()), None, "nothing yet");
        let first = Lock::take(dir.path(), "the web app").unwrap();
        let by = Lock::held_by(dir.path()).unwrap();
        assert!(by.starts_with("the web app (process "), "{by}");
        assert!(Lock::take(dir.path(), "x").is_err(), "held_by took nothing");
        let err = Lock::take(dir.path(), "`ptyro install` in a terminal").unwrap_err();
        assert!(err.starts_with("setup is already running (from the web app (process "), "{err}");
        drop(first);
        assert!(!dir.path().join("setup.who").exists());
        assert_eq!(Lock::held_by(dir.path()), None);
        let again = Lock::take(dir.path(), "`ptyro setup`").unwrap();
        drop(again);
    }

    /// A probe at the same instant as another (two checklists looking, two test threads) is no
    /// holder: probes share the lock.
    #[test]
    fn a_probe_beside_another_probe_finds_it_free() {
        let dir = tempfile::tempdir().unwrap();
        drop(Lock::take(dir.path(), "x").unwrap());
        let other = OpenOptions::new().write(true).open(dir.path().join("setup.lock")).unwrap();
        other.try_lock_shared().unwrap();
        assert_eq!(Lock::held_by(dir.path()), None);
        drop(other);
    }

    /// Only the OS lock decides: a `setup.who` left behind naming a live, unrelated process (a
    /// reused id; here this test's own) doesn't block, and is tidied away.
    #[test]
    fn a_stale_note_naming_a_live_process_never_blocks() {
        let dir = tempfile::tempdir().unwrap();
        drop(Lock::take(dir.path(), "x").unwrap());
        let who = dir.path().join("setup.who");
        std::fs::write(&who, format!("`ptyro install` (process {})", std::process::id())).unwrap();
        assert_eq!(Lock::held_by(dir.path()), None);
        assert!(!who.exists(), "the stale note is gone");
        std::fs::write(&who, format!("`ptyro install` (process {})", std::process::id())).unwrap();
        let held = Lock::take(dir.path(), "`ptyro update`").expect("free: taken");
        assert!(Lock::held_by(dir.path()).unwrap().starts_with("`ptyro update` (process "));
        drop(held);
    }

    /// This process's own lock is said without being probed: asking never lets go of it (POSIX
    /// locks, as NFS emulates `flock` with, belong to the process).
    #[test]
    fn asking_in_the_holding_process_shows_the_holder_and_keeps_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let held = Lock::take(dir.path(), "the web app, through the daemon").unwrap();
        for _ in 0..3 {
            let by = Lock::held_by(dir.path()).expect("held");
            assert!(by.starts_with("the web app, through the daemon (process "), "{by}");
        }
        assert!(dir.path().join("setup.who").exists(), "its note is kept");
        assert!(Lock::take(dir.path(), "`ptyro install`").is_err(), "still held");
        drop(held);
        assert_eq!(Lock::held_by(dir.path()), None);
        drop(Lock::take(dir.path(), "`ptyro install`").unwrap());
    }

    /// A probe racing a take never makes the take refuse.
    #[test]
    fn a_probe_racing_a_take_never_refuses_it() {
        let dir = tempfile::tempdir().unwrap();
        drop(Lock::take(dir.path(), "x").unwrap());
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (d, s) = (dir.path().to_path_buf(), stop.clone());
        let prober = std::thread::spawn(move || {
            while !s.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = Lock::held_by(&d);
            }
        });
        for _ in 0..200 {
            let l = Lock::take(dir.path(), "`ptyro install`").expect("never refused for a probe");
            drop(l);
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        prober.join().unwrap();
    }
}
