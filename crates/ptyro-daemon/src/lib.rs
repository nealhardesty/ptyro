//! The ptyro daemon (SPEC §4): owns every PTY and terminal state, serves clients over a Unix
//! socket (a named pipe on Windows, WIN5), and keeps sessions alive while no client is attached.

mod again;
mod agents;
mod approvals;
mod autoreg;
mod background;
mod changesets;
mod chats;
mod conn;
mod daemon;
mod devices;
mod diag;
mod engine;
mod evidence;
mod finish;
mod github;
mod handoff;
mod launch;
mod listen;
mod machines;
mod mail;
mod meaning;
mod notifier;
mod push;
mod quick;
mod registry;
mod review;
mod runs;
mod scratchpad;
mod serve_child;
mod session;
mod shells;
mod sidecar;
mod structured;
mod suggest;
mod summary;
mod usage;
mod verify;
mod watchdog;
mod web;
mod worktrees;
mod wsl_hold;

use std::fs::{File, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ptyro_core::config::Config;
use ptyro_core::paths::Paths;
use ptyro_store::Store;

pub use daemon::Daemon;

/// Handoff image versions this binary can take over from (`ptyro daemon handoff-version`).
pub const HANDOFF_VERSIONS: &[u32] = &[handoff::IMAGE_VERSION];

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Also log to stderr (`ptyro daemon start --foreground`).
    pub foreground: bool,
}

/// Fuzzing and tests: read a handoff image the way an upgraded daemon does (the MessagePack
/// image, then each session's compressed emulator), without adopting any file descriptor.
#[doc(hidden)]
pub fn decode_handoff_image(bytes: &[u8]) -> Result<usize, String> {
    let image: handoff::Image = rmp_serde::from_slice(bytes).map_err(|e| e.to_string())?;
    for s in &image.sessions {
        session::Session::decode_emulator(s).map_err(|e| e.to_string())?;
    }
    Ok(image.sessions.len())
}

/// The same for one session's compressed emulator image (what `SessionImage::emu` holds).
#[doc(hidden)]
pub fn decode_handoff_emulator(bytes: &[u8]) -> Result<(), String> {
    let raw = zstd::stream::decode_all(bytes).map_err(|e| e.to_string())?;
    let image: ptyro_term::emulator::EmuImage =
        rmp_serde::from_slice(&raw).map_err(|e| e.to_string())?;
    drop(ptyro_term::AlacrittyEmulator::from_image(image));
    Ok(())
}

/// Run the daemon until it is told to stop. Blocks; builds its own tokio runtime.
pub fn run(opts: Options) -> Result<()> {
    let paths = Paths::from_env().context("resolve paths")?;
    prepare_dirs(&paths)?;
    let _log_guard = init_logging(&paths, opts.foreground)?;
    watchdog::install_panic_hook(&paths.state);
    // A live upgrade's descriptors (the lock among them) come over a socket (H2.7): first.
    #[cfg(unix)]
    match ptyro_pty::handoff::receive_carried() {
        Ok(0) => {}
        Ok(n) => tracing::info!("{n} descriptor(s) carried over from the previous image"),
        Err(e) => tracing::error!("carried descriptors: {e}"),
    }
    let lock = acquire_lock(&paths)?;
    #[cfg(unix)]
    LOCK_FD.store(std::os::fd::AsRawFd::as_raw_fd(&lock), std::sync::atomic::Ordering::SeqCst);
    // A live upgrade hands us the previous image (or re-execs the old binary if we can't read it).
    let inherited = handoff::take_image(&paths.state);
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let result = runtime.block_on(serve(paths, opts.foreground, inherited));
    drop(lock);
    if let Err(e) = &result {
        tracing::error!("daemon failed: {e:#}");
    }
    result
}

fn prepare_dirs(paths: &Paths) -> Result<()> {
    for dir in [&paths.config, &paths.data, &paths.state] {
        ptyro_core::os::create_private_dirs(dir)
            .with_context(|| format!("create {}", dir.display()))?;
    }
    // The socket directory may live in a shared /tmp: insist it is ours and private (SPEC §17).
    listen::prepare(&paths.socket)
}

/// Changes the log filter while the daemon runs (`[daemon] log_level`, set by `init_logging`).
static LOG_FILTER: std::sync::OnceLock<
    tracing_subscriber::reload::Handle<tracing_subscriber::EnvFilter, tracing_subscriber::Registry>,
> = std::sync::OnceLock::new();

/// Apply `[daemon] log_level`. `PTYRO_LOG`, when set, is the person's explicit choice and wins.
pub(crate) fn set_log_level(level: &str) {
    if std::env::var_os("PTYRO_LOG").is_some() {
        return;
    }
    if let (Some(handle), Ok(filter)) = (
        LOG_FILTER.get(),
        tracing_subscriber::EnvFilter::try_new(level.trim().to_ascii_lowercase()),
    ) {
        let _ = handle.reload(filter);
    }
}

fn init_logging(
    paths: &Paths,
    foreground: bool,
) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, fmt};
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("daemon")
        .filename_suffix("log")
        .max_log_files(7)
        .build(paths.log_dir())
        .context("open daemon log")?;
    let (file, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_env("PTYRO_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let (filter, handle) = tracing_subscriber::reload::Layer::new(filter);
    let _ = LOG_FILTER.set(handle);
    let stderr = foreground.then(|| fmt::layer().with_writer(std::io::stderr));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_ansi(false).with_writer(file))
        .with(stderr)
        .try_init()
        .ok();
    Ok(guard)
}

/// `daemon.lock`'s descriptor, for a live upgrade to carry over.
#[cfg_attr(windows, allow(dead_code))] // the live upgrade's: Unix until WIN9
pub(crate) static LOCK_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// Nothing is carried over on Windows until its live upgrade (WIN9).
#[cfg(windows)]
fn carried_lock(_path: &Path) -> Option<File> {
    None
}

/// The lock the previous image held, carried over by a live upgrade (H2.7): never let go across the
/// exec. Only when it really is `daemon.lock` (the same file).
#[cfg(unix)]
fn carried_lock(path: &Path) -> Option<File> {
    use std::os::unix::fs::MetadataExt;
    let raw: i32 = std::env::var(handoff::ENV_LOCK).ok()?.parse().ok()?;
    let f = File::from(ptyro_pty::handoff::adopt(raw).ok()?);
    let (a, b) = (f.metadata().ok()?, std::fs::metadata(path).ok()?);
    (a.dev() == b.dev() && a.ino() == b.ino()).then_some(f)
}

/// One daemon per user: an exclusive lock on `state/daemon.lock`, which also records our pid.
fn acquire_lock(paths: &Paths) -> Result<File> {
    let path = paths.lock_file();
    let mut f = match carried_lock(&path) {
        Some(f) => f,
        None => File::options().create(true).truncate(false).read(true).write(true).open(&path)?,
    };
    // A carried lock is already ours: locking it again (the same open file) just succeeds.
    match f.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let mut pid = String::new();
            let _ = f.read_to_string(&mut pid);
            bail!("daemon already running (pid {})", pid.trim());
        }
        Err(TryLockError::Error(e)) => return Err(e).context("lock daemon.lock"),
    }
    f.set_len(0)?;
    f.rewind()?;
    writeln!(f, "{}", std::process::id())?;
    Ok(f)
}

/// Reload the config when its file changes (SPEC §13.1). Watches the directory, so editors that
/// replace the file and a file created later both work.
fn watch_config(
    daemon: &Arc<Daemon>,
    path: &Path,
) -> Option<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>> {
    let dir = path.parent()?.to_path_buf();
    let _ = std::fs::create_dir_all(&dir);
    // Events name the real path: macOS's FSEvents reports `/private/var/…` for `/var/…`, so a
    // config dir reached through a symlink would never match.
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    let target = dir.join(path.file_name()?);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut debouncer = notify_debouncer_mini::new_debouncer(
        Duration::from_millis(200),
        move |res: notify_debouncer_mini::DebounceEventResult| {
            if let Ok(events) = res
                && events.iter().any(|e| e.path == target)
            {
                let _ = tx.send(());
            }
        },
    )
    .map_err(|e| tracing::warn!("config watch unavailable: {e}"))
    .ok()?;
    if let Err(e) = debouncer.watcher().watch(&dir, notify::RecursiveMode::NonRecursive) {
        tracing::warn!("cannot watch {}: {e}", dir.display());
        return None;
    }
    let daemon = Arc::clone(daemon);
    let path = path.to_path_buf();
    tokio::spawn(async move {
        // Reading the file is itself a file event (inotify reports opens), as is every reader's
        // (TUIs re-read it on `ConfigReloaded`): reload only when the content really changed, or
        // the daemon and its TUIs would ping-pong forever.
        let mut last = std::fs::read(&path).ok();
        while rx.recv().await.is_some() {
            let now = std::fs::read(&path).ok();
            if now != last {
                last = now;
                let _ = daemon.reload_config();
            }
        }
    });
    Some(debouncer)
}

/// What stops the daemon from outside: SIGTERM or SIGINT (`ptyro daemon stop` asks over the
/// socket instead). SIGHUP is ignored: no controlling terminal to lose, so a stray one can't take
/// sessions down.
#[cfg(unix)]
async fn stop_signal() -> Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    let mut hup = signal(SignalKind::hangup())?;
    loop {
        tokio::select! {
            _ = term.recv() => return Ok(()),
            _ = int.recv() => return Ok(()),
            _ = hup.recv() => {}
        }
    }
}

/// On Windows: Ctrl+C or Ctrl+Break in the daemon's console (`--foreground`), its console
/// closing, or Windows shutting down. A detached daemon has no console: `ptyro daemon stop`.
#[cfg(windows)]
async fn stop_signal() -> Result<()> {
    use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_shutdown};
    let (mut c, mut b) = (ctrl_c()?, ctrl_break()?);
    let (mut close, mut shutdown) = (ctrl_close()?, ctrl_shutdown()?);
    tokio::select! {
        _ = c.recv() => {}
        _ = b.recv() => {}
        _ = close.recv() => {}
        _ = shutdown.recv() => {}
    }
    Ok(())
}

async fn serve(
    paths: Paths,
    foreground: bool,
    inherited: Option<handoff::Inherited>,
) -> Result<()> {
    let listener = match &inherited {
        // The socket never closed: clients that connected meanwhile wait in its backlog.
        #[cfg(unix)]
        Some(i) => listen::Listener::inherited(
            ptyro_pty::handoff::adopt(i.image.listener_fd).context("inherited socket")?,
            &paths.socket,
        )?,
        _ => listen::Listener::bind(&paths.socket)?,
    };
    let socket = paths.socket.clone();
    let config_path = Config::path(&paths);
    let mut config_error = None;
    let config = match Config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            // A broken config must never keep sessions from starting: run on defaults, say why.
            tracing::error!("{e}; using defaults");
            config_error =
                Some(format!("the config didn't load, so the daemon runs on defaults: {e}"));
            Config::default()
        }
    };
    set_log_level(&config.daemon.log_level);
    let db = paths.data.join("ptyro.db");
    let mut store_error = None;
    let store = match Store::open(&db) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::error!(
                "store {} unavailable: {e}; session history and saved views are off",
                db.display()
            );
            // Said where people look, not only in the log: what it means and what to do.
            store_error = Some(format!(
                "ptyro's database isn't available, so session history and saved views are off: {e}"
            ));
            None
        }
    };
    let registry = match &store {
        Some(s) => s.load_registry().await.unwrap_or_else(|e| {
            tracing::error!("store: cannot load projects ({e}); starting empty");
            ptyro_store::Registry::default()
        }),
        None => ptyro_store::Registry::default(),
    };
    let identity = inherited.as_ref().map(|i| (i.image.run_id.clone(), i.image.started_ms));
    let daemon = Daemon::with_identity(paths, store, config, registry, identity);
    daemon.listener_fd.store(listener.raw(), std::sync::atomic::Ordering::SeqCst);
    daemon.foreground.store(foreground, std::sync::atomic::Ordering::SeqCst);
    if let Some(mut i) = inherited {
        use std::sync::atomic::Ordering::SeqCst;
        daemon.generation.store(i.image.generation, SeqCst);
        daemon.handed_off.store(true, SeqCst);
        daemon.rolled_back.store(i.rolled_back, SeqCst);
        daemon.upgraded_ms.store(ptyro_core::now_ms(), SeqCst);
        watchdog::restore_counters(i.image.counters);
        let n = i.image.sessions.len();
        daemon.adopt_sessions(&mut i.image);
        tracing::info!(
            sessions = n,
            from = %i.image.binary,
            took_ms = ptyro_core::now_ms().saturating_sub(i.image.written_ms),
            rolled_back = i.rolled_back,
            "took over from the previous daemon image"
        );
        if i.rolled_back {
            daemon.warn(
                "the last daemon upgrade was refused by the new binary and rolled back".into(),
            );
        }
    }
    if let Some(e) = config_error {
        daemon.warn(e);
    }
    // Said where people look (doctor, the TUI's health badge, the web's Health page): sessions
    // here run in Windows' own ConPTY, which repaints what programs write (SPEC §20.13).
    #[cfg(windows)]
    if ptyro_pty::packaged_conpty().is_none() {
        daemon.warn(
            "sessions use Windows' own ConPTY, which repaints programs' output: status can arrive \
             out of order and colour queries go unanswered. ptyro.exe's folder should hold \
             conpty.dll and OpenConsole.exe: install ptyro again (`irm \
             https://raw.githubusercontent.com/nealhardesty/ptyro/master/scripts/install.ps1 | iex`)"
                .into(),
        );
    }
    if let Some(e) = store_error {
        daemon.warn(e);
    }
    daemon.ensure_main_workspace();
    daemon.reconcile().await;
    daemon.lag.start();
    daemon.start_background();
    daemon.start_agents();
    daemon.start_pr_poller();
    daemon.start_review_ticks();
    daemon.start_archive_purge();
    if let Some(store) = &daemon.store {
        match store.close_orphans(daemon.run_id.clone()).await {
            Ok(n) if n > 0 => {
                tracing::info!("{n} session(s) from an earlier daemon are now history")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("store: {e}"),
        }
        if let Err(e) = store.prune().await {
            tracing::warn!("store prune: {e}");
        }
        daemon.prune_recordings().await;
    }
    daemon.load_devices().await;
    daemon.load_used_links().await;
    daemon.load_last_launch().await;
    daemon.start_machines().await;
    daemon.load_declined().await;
    daemon.load_integrated().await;
    daemon.start_serve_supervisor();
    daemon.start_wsl_hold();
    daemon.start_pad_watcher();
    daemon.start_chats().await;
    // Retention (SPEC §14.2) is applied once a day while the daemon runs, not only at its start.
    {
        let d = Arc::clone(&daemon);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(24 * 3600)).await;
                if let Some(store) = &d.store
                    && let Err(e) = store.prune().await
                {
                    tracing::warn!("store prune: {e}");
                }
                d.prune_recordings().await;
            }
        });
    }
    daemon.restore_dormant().await;
    daemon.restore_shells().await;
    daemon.start_auto_register();
    daemon.start_shells();
    daemon.start_mail().await;
    daemon.start_runs().await;
    let _watcher = watch_config(&daemon, &config_path);
    tracing::info!(pid = std::process::id(), socket = %socket.display(), "daemon started");

    let stop = stop_signal();
    tokio::pin!(stop);
    let mut stop_seen = false;
    let mut stopping = daemon.shutdown.subscribe();
    let mut goodbye = daemon.goodbye.subscribe();
    loop {
        let upgrading = daemon.upgrading.load(std::sync::atomic::Ordering::SeqCst);
        tokio::select! {
            // While handing off, new clients wait in the backlog for the next image.
            _ = goodbye.changed() => {}
            (stream, pid) = listener.accept(), if !upgrading => {
                tokio::spawn(conn::serve(Arc::clone(&daemon), stream, pid));
            }
            r = &mut stop, if !stop_seen => {
                stop_seen = true;
                if let Err(e) = r {
                    tracing::warn!("signal handling: {e}");
                }
                let _ = daemon.shutdown.send(true);
            }
            () = async { let _ = stopping.wait_for(|s| *s).await; } => break,
        }
    }
    tracing::info!("shutting down");
    daemon.serve_child.kill();
    daemon.wsl_hold.kill();
    listener.close();
    // SIGTERM means `stop --hibernate` (SPEC §4.4); until hibernation exists (M4), stop gracefully.
    daemon.stop_all().await;
    if let Some(store) = &daemon.store {
        let _ = store.flush().await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    tracing::info!("bye");
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use listen::bind;

    #[tokio::test]
    async fn a_socket_is_replaced_only_when_nobody_listens_and_removed_only_if_ours() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("ptyro.sock");
        // Someone listens: refused.
        let live = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let err = bind(&path).unwrap_err().to_string();
        assert!(err.contains("in use"), "{err}");
        // Gone quiet (closed, the file left behind): a refused connection proves it stale.
        drop(live);
        // (A busy machine can take a moment to start refusing connections to a closed socket;
        // until it does, `bind` rightly leaves the file alone.)
        let start = std::time::Instant::now();
        let second = loop {
            match listen::Listener::bind(&path) {
                Ok(l) => break l,
                Err(e) if start.elapsed() < Duration::from_secs(5) => {
                    assert!(e.to_string().contains("in use"), "{e}");
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("{e}"),
            }
        };
        // Another daemon replaced the file meanwhile: leave its socket alone. (The old file is
        // kept linked while that happens: a file system may hand a freed inode number straight
        // to the next file made, and then "another file" would look like ours.)
        std::fs::hard_link(&path, d.path().join("old.sock")).unwrap();
        std::fs::remove_file(&path).unwrap();
        let third = bind(&path).unwrap();
        second.close();
        assert!(path.exists(), "not ours any more");
        // Ours again (the other one went quiet): removed on the way out.
        drop(third);
        let fourth = loop {
            match listen::Listener::bind(&path) {
                Ok(l) => break l,
                Err(_) if start.elapsed() < Duration::from_secs(10) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("{e}"),
            }
        };
        fourth.close();
        assert!(!path.exists());
    }
}
