//! `ptyro backup` (ROADMAP H2.3): a `.tar.gz` of ptyro's own state, so a machine can be moved or an
//! uninstall undone by hand. It holds the config, a consistent copy of the database (sessions'
//! history, projects, worktree records, paired devices as token hashes, saved views, trust), the
//! scratchpad and its history. It leaves out what is a secret or somebody else's: the agents'
//! login directories under managed profiles, the web app's certificate authority key (`--with-ca`
//! adds it), and the recordings, which are big and can hold what was on screen
//! (`--with-recordings`). The file is private (0600) and never replaces one that is there.

use std::path::{Path, PathBuf};

use ptyro_core::Error;
use ptyro_core::config::Config;
use serde::Serialize;

use crate::cli::Ctx;

#[derive(Debug, Clone, Default)]
pub struct Opts {
    pub output: Option<PathBuf>,
    pub with_recordings: bool,
    pub with_ca: bool,
}

/// What a backup made.
#[derive(Debug, Clone, Serialize)]
pub struct Made {
    pub path: PathBuf,
    pub bytes: u64,
    /// What is in it, by name.
    pub items: Vec<String>,
    /// What was left out, and why.
    pub left_out: Vec<String>,
}

/// `20261025-143007` for a time in seconds since 1970, in UTC (no calendar crate needed for it).
pub fn stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Where a backup goes when nobody says: your home directory.
pub fn default_output() -> Result<PathBuf, Error> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error::invalid("HOME isn't set: say where with -o"))?;
    Ok(home.join(format!("ptyro-backup-{}.tar.gz", stamp(ptyro_core::now_ms() / 1000))))
}

fn readme(version: &str, items: &[String], left_out: &[String]) -> String {
    format!(
        "This is a backup of ptyro {version}'s own state, made with `ptyro backup`.\n\n\
         It holds:\n{}\n\
         It does not hold:\n{}\n\n\
         To put it back by hand (with ptyro stopped: `ptyro daemon stop`), unpack it somewhere and copy:\n\
         \x20 config/config.toml          to  `ptyro config path`'s file\n\
         \x20 data/ptyro.db               to  ptyro's data directory (`ptyro daemon status` shows its logs\n\
         \x20                                 directory; the data directory is beside it, usually\n\
         \x20                                 ~/.local/share/ptyro), replacing ptyro.db there\n\
         \x20 data/scratchpad.md and data/scratchpad.history/   likewise\n\
         Worktrees are not in it: they are in your repositories (their branches), and `ptyro wt restore`\n\
         brings archived ones back from the snapshots in your repositories.\n\
         The file is private to you (mode 600): it names your projects and paired devices.\n",
        items.iter().map(|i| format!("  - {i}\n")).collect::<String>(),
        left_out.iter().map(|i| format!("  - {i}\n")).collect::<String>().trim_end(),
    )
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<u64> {
    let mut bytes = 0;
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let meta = std::fs::symlink_metadata(&src)?;
        if meta.is_dir() {
            bytes += copy_tree(&src, &dst)?;
        } else if meta.is_file() {
            bytes += std::fs::copy(&src, &dst)?;
        } // links are not followed or copied: a backup holds files, not where they pointed
    }
    Ok(bytes)
}

fn daemon_running(ctx: &Ctx) -> bool {
    std::fs::File::open(ctx.paths.lock_file()).is_ok_and(|f| f.try_lock_shared().is_err())
}

/// Make the backup.
pub async fn create(ctx: &Ctx, o: &Opts) -> Result<Made, Error> {
    if ctx.remote.is_some() {
        return Err(Error::invalid("backup works on this computer's own files, not over --remote"));
    }
    let out = match &o.output {
        Some(p) => p.clone(),
        None => default_output()?,
    };
    if std::fs::symlink_metadata(&out).is_ok() {
        return Err(Error::invalid(format!(
            "{} is already there: backups never replace a file (pick another with -o)",
            out.display()
        )));
    }
    let io = |what: &str, e: std::io::Error| Error::internal(format!("{what}: {e}"));
    let stage = tempfile::Builder::new()
        .prefix("ptyro-backup-")
        .tempdir()
        .map_err(|e| io("a temporary directory", e))?;
    let root = stage.path().join("ptyro-backup");
    std::fs::create_dir_all(root.join("config")).map_err(|e| io("staging", e))?;
    std::fs::create_dir_all(root.join("data")).map_err(|e| io("staging", e))?;
    let (mut items, mut left_out) = (Vec::new(), Vec::new());
    let paths = &ctx.paths;

    // The config.
    let cfg_path = Config::path(paths);
    if cfg_path.exists() {
        std::fs::copy(&cfg_path, root.join("config/config.toml")).map_err(|e| io("config", e))?;
        items.push("config/config.toml: your config".to_owned());
    }
    let config = Config::load(&cfg_path).unwrap_or_default();

    // The database: a consistent copy, even while the daemon writes it.
    let db = paths.data.join("ptyro.db");
    if db.exists() {
        let copy = root.join("data/ptyro.db");
        let via_store = match ptyro_store::Store::open(&db) {
            Ok(store) => store.backup_to(copy.clone()).await.is_ok(),
            Err(_) => false,
        };
        if !via_store {
            // A database this build can't open (a newer ptyro's): the files as they are, with the
            // write-ahead log SQLite needs to read them whole. Only safe with the daemon stopped.
            if daemon_running(ctx) {
                return Err(Error::internal(
                    "couldn't copy the database while the daemon runs it: stop the daemon (`ptyro daemon stop`) and try again",
                ));
            }
            for suffix in ["", "-wal", "-shm"] {
                let from = paths.data.join(format!("ptyro.db{suffix}"));
                if from.exists() {
                    std::fs::copy(&from, root.join(format!("data/ptyro.db{suffix}")))
                        .map_err(|e| io("database", e))?;
                }
            }
        }
        items.push(
            "data/ptyro.db: sessions' history, projects, worktree records, devices (token hashes), saved views, trust".to_owned(),
        );
    }

    // The scratchpad and its saved copies.
    let pad = config.scratchpad_path(&paths.data);
    if pad.is_file() {
        std::fs::copy(&pad, root.join("data/scratchpad.md")).map_err(|e| io("scratchpad", e))?;
        items.push("data/scratchpad.md: the scratchpad".to_owned());
    }
    let history = paths.data.join("scratchpad.history");
    if history.is_dir() {
        copy_tree(&history, &root.join("data/scratchpad.history")).map_err(|e| io("history", e))?;
        items.push("data/scratchpad.history/: its saved copies".to_owned());
    }

    // Opt-ins.
    let sessions = paths.data.join("sessions");
    if o.with_recordings && sessions.is_dir() {
        copy_tree(&sessions, &root.join("data/sessions")).map_err(|e| io("recordings", e))?;
        items.push("data/sessions/: every session's recording".to_owned());
    } else if sessions.is_dir() {
        left_out.push(
            "recordings (data/sessions): big, and they hold what was on screen; --with-recordings"
                .to_owned(),
        );
    }
    let tls = paths.data.join("serve").join("tls");
    if o.with_ca && tls.is_dir() {
        copy_tree(&tls, &root.join("data/serve/tls")).map_err(|e| io("certificates", e))?;
        items.push(
            "data/serve/tls/: the web app's certificate authority, with its PRIVATE KEY".to_owned(),
        );
    } else if tls.is_dir() {
        left_out.push("the web app's certificate authority and its private key (data/serve/tls); --with-ca. Without it a restored ptyro makes a new one and phones trust it again".to_owned());
    }
    if paths.data.join("profiles").is_dir() {
        left_out.push(
            "agent logins in ptyro-managed profiles (data/profiles): log in again after a restore"
                .to_owned(),
        );
    }

    let version = env!("CARGO_PKG_VERSION");
    std::fs::write(root.join("README.txt"), readme(version, &items, &left_out))
        .map_err(|e| io("README", e))?;
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "ptyro": version, "made_ms": ptyro_core::now_ms(), "items": items, "left_out": left_out,
            "config": cfg_path, "data": paths.data,
        }))
        .unwrap_or_default(),
    )
    .map_err(|e| io("manifest", e))?;

    // Packed to a private file under a temporary name, then linked into place: if somebody made
    // the name meanwhile, theirs stays.
    let dir = out.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| io("the destination folder", e))?;
    let part = dir.join(format!(".ptyro-backup-{}.part", std::process::id()));
    {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&part)
            .map_err(|e| io(&part.display().to_string(), e))?;
        let status = std::process::Command::new("tar")
            .args(["-czf", "-", "-C"])
            .arg(stage.path())
            .arg("ptyro-backup")
            .stdout(file)
            .status();
        match status {
            Ok(s) if s.success() => {}
            other => {
                let _ = std::fs::remove_file(&part);
                return Err(Error::internal(format!(
                    "`tar` failed ({other:?}): backups need the system tar"
                )));
            }
        }
    }
    let placed = std::fs::hard_link(&part, &out);
    let _ = std::fs::remove_file(&part);
    placed.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            Error::invalid(format!("{} appeared meanwhile: not replaced", out.display()))
        } else {
            io(&out.display().to_string(), e)
        }
    })?;
    let bytes = std::fs::metadata(&out).map_or(0, |m| m.len());
    Ok(Made { path: out, bytes, items, left_out })
}

/// `ptyro backup`.
pub async fn run(ctx: &Ctx, o: Opts, json: bool) -> Result<i32, Error> {
    let made = create(ctx, &o).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&made).unwrap_or_default());
        return Ok(0);
    }
    println!("{} ({})", made.path.display(), crate::uninstall::human(made.bytes));
    for i in &made.items {
        println!("  + {i}");
    }
    for i in &made.left_out {
        println!("  - not included: {i}");
    }
    println!("Private to you (mode 600). Its README.txt says how to put it back.");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_utc_dates_and_times() {
        assert_eq!(stamp(0), "19700101-000000");
        assert_eq!(stamp(1_792_903_807), "20261025-045007");
        // A leap day, and the last second of a year.
        assert_eq!(stamp(1_709_164_800), "20240229-000000");
        assert_eq!(stamp(1_735_689_599), "20241231-235959");
    }

    #[test]
    fn the_readme_says_what_is_and_is_not_in_it() {
        let r = readme("0.0.4", &["a".into()], &["b".into()]);
        assert!(r.contains("- a") && r.contains("- b") && r.contains("ptyro 0.0.4"));
        assert!(r.contains("stopped"), "restoring is done with ptyro stopped");
    }
}
