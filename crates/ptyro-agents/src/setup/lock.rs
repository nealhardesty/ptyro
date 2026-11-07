//! One install at a time on a machine, whoever starts it (BOOT3 review, SPEC §24.6): `ptyro
//! install`, `ptyro setup` (the CLI, and the TUI's splits that run it) and the daemon's
//! `SetupRun` (the web) each hold `<state>/setup.lock` while they install, so two package
//! managers never fight over their locks. The holder says who it is in `<state>/setup.who` (a
//! file of its own: Windows' locks keep others from reading a locked file), so a second run says
//! where the first came from. The lock is the operating system's (`File::try_lock`: `flock` on
//! Unix, `LockFileEx` on Windows), so it is let go however the holder ends, a crash included.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

/// Holds the machine's setup lock until dropped.
#[derive(Debug)]
pub struct Lock {
    /// `None` when the filesystem can't lock at all: installs go ahead unguarded, as before.
    _file: Option<File>,
    who: PathBuf,
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
            Err(_) => return Ok(Self { _file: None, who: who_path }),
        };
        match file.try_lock() {
            Ok(()) => {
                let _ =
                    std::fs::write(&who_path, format!("{who} (process {})", std::process::id()));
                // A version check going beside this won't write over what it installs (BOOT5).
                crate::updates::started(state);
                Ok(Self { _file: Some(file), who: who_path })
            }
            Err(TryLockError::WouldBlock) => Err(busy(&holder(&who_path))),
            Err(TryLockError::Error(_)) => Ok(Self { _file: None, who: who_path }),
        }
    }

    /// Who holds the lock in `state` now ("`ptyro install` (process 4242)"); `None` when it is
    /// free. For a checklist to say so before anyone ticks anything. It never touches the lock
    /// (asking can't make a `ptyro install` starting that instant be refused): it reads what the
    /// holder wrote of itself, `setup.who`, and checks that its process still runs (a holder that
    /// crashed leaves the file behind, but the lock went with it).
    pub fn held_by(state: &Path) -> Option<String> {
        let text = std::fs::read_to_string(state.join("setup.who")).ok()?;
        let text = text.trim().to_owned();
        let pid: u32 = text
            .rsplit_once("(process ")
            .and_then(|(_, rest)| rest.trim_end_matches(')').trim().parse().ok())?;
        alive(pid).then_some(text)
    }
}

/// Whether process `pid` is running.
fn alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        ptyro_pty::alive(pid)
    }
    #[cfg(not(windows))]
    {
        ptyro_core::os::alive(pid)
    }
}

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
        // A holder that crashed leaves its note behind, but not the lock: free.
        let gone = dir.path().join("gone");
        std::fs::create_dir_all(&gone).unwrap();
        std::fs::write(gone.join("setup.who"), "`ptyro install` (process 4294967294)").unwrap();
        assert_eq!(Lock::held_by(&gone), None);
        let err = Lock::take(dir.path(), "`ptyro install` in a terminal").unwrap_err();
        assert!(err.starts_with("setup is already running (from the web app (process "), "{err}");
        drop(first);
        assert!(!dir.path().join("setup.who").exists());
        assert_eq!(Lock::held_by(dir.path()), None);
        let again = Lock::take(dir.path(), "`ptyro setup`").unwrap();
        drop(again);
    }
}
