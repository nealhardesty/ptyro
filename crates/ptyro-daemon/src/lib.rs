//! The ptyro daemon (SPEC §4): owns every PTY and terminal state, serves clients over a Unix
//! socket, and keeps sessions alive while no client is attached.

mod agents;
mod approvals;
mod autoreg;
mod background;
mod changesets;
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
mod mail;
mod notifier;
mod push;
mod quick;
mod registry;
mod review;
mod runs;
mod scratchpad;
mod serve_child;
mod session;
mod sidecar;
mod structured;
mod suggest;
mod summary;
mod usage;
mod verify;
mod watchdog;
mod web;
mod worktrees;

use std::fs::{DirBuilder, File, TryLockError};
use std::io::{Read, Seek, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ptyro_core::config::Config;
use ptyro_core::paths::Paths;
use ptyro_store::Store;
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};

pub use daemon::Daemon;

/// Handoff image versions this binary can take over from (`ptyro daemon handoff-version`).
pub const HANDOFF_VERSIONS: &[u32] = &[handoff::IMAGE_VERSION];

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Also log to stderr (`ptyro daemon start --foreground`).
    pub foreground: bool,
}

/// Run the daemon until it is told to stop. Blocks; builds its own tokio runtime.
pub fn run(opts: Options) -> Result<()> {
    let paths = Paths::from_env().context("resolve paths")?;
    prepare_dirs(&paths)?;
    let _log_guard = init_logging(&paths, opts.foreground)?;
    watchdog::install_panic_hook(&paths.state);
    let lock = acquire_lock(&paths)?;
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
    let mut b = DirBuilder::new();
    b.recursive(true).mode(0o700);
    for dir in [&paths.config, &paths.data, &paths.state] {
        b.create(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let sock_dir = paths.socket.parent().context("socket path has no parent")?;
    b.create(sock_dir).with_context(|| format!("create {}", sock_dir.display()))?;
    // The socket directory may live in a shared /tmp: insist it is ours and private (SPEC §17).
    let meta = std::fs::metadata(sock_dir)?;
    let uid = rustix::process::getuid().as_raw();
    if meta.uid() != uid {
        bail!(
            "{} is owned by uid {}, not us ({uid}); refusing to use it",
            sock_dir.display(),
            meta.uid()
        );
    }
    if meta.mode() & 0o077 != 0 {
        std::fs::set_permissions(sock_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
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

/// One daemon per user: an exclusive lock on `state/daemon.lock`, which also records our pid.
fn acquire_lock(paths: &Paths) -> Result<File> {
    let path = paths.lock_file();
    let mut f = File::options().create(true).truncate(false).read(true).write(true).open(&path)?;
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

/// The socket file's identity, to remove it on the way out only if it is still this daemon's.
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

fn remove_if_ours(path: &Path, ours: Option<(u64, u64)>) {
    if ours.is_some() && file_id(path) == ours {
        let _ = std::fs::remove_file(path);
    }
}

fn bind(socket: &Path) -> Result<UnixListener> {
    if socket.exists() {
        // Only a refused connection proves nobody listens. Any other failure (a sandbox that
        // forbids connecting, say) leaves the socket alone: it may be another daemon's.
        match std::os::unix::net::UnixStream::connect(socket) {
            Ok(_) => bail!("{} is in use by another process", socket.display()),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                std::fs::remove_file(socket)
                    .with_context(|| format!("remove stale {}", socket.display()))?;
            }
            Err(e) => bail!(
                "{} exists and can't be checked ({e}); not replacing it (another daemon may use it)",
                socket.display()
            ),
        }
    }
    let old = rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    let listener = UnixListener::bind(socket);
    rustix::process::umask(old);
    let listener = listener.with_context(|| format!("bind {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

async fn serve(
    paths: Paths,
    foreground: bool,
    inherited: Option<handoff::Inherited>,
) -> Result<()> {
    use std::os::fd::AsRawFd;
    let listener = match &inherited {
        // The socket never closed: clients that connected meanwhile wait in its backlog.
        Some(i) => {
            let fd = ptyro_pty::handoff::adopt(i.image.listener_fd).context("inherited socket")?;
            let std = std::os::unix::net::UnixListener::from(fd);
            std.set_nonblocking(true)?;
            UnixListener::from_std(std)?
        }
        None => bind(&paths.socket)?,
    };
    let socket = paths.socket.clone();
    let socket_id = file_id(&socket);
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
    let store = match Store::open(&db) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::error!(
                "store {} unavailable: {e}; session history and saved views are off",
                db.display()
            );
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
    daemon.listener_fd.store(listener.as_raw_fd(), std::sync::atomic::Ordering::SeqCst);
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
    daemon.ensure_main_workspace();
    daemon.reconcile().await;
    daemon.lag.start();
    daemon.start_background();
    daemon.start_agents();
    daemon.start_pr_poller();
    daemon.start_review_ticks();
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
    daemon.load_declined().await;
    daemon.load_integrated().await;
    daemon.start_serve_supervisor();
    daemon.start_pad_watcher();
    daemon.restore_dormant().await;
    daemon.start_auto_register();
    daemon.start_mail().await;
    daemon.start_runs().await;
    let _watcher = watch_config(&daemon, &config_path);
    tracing::info!(pid = std::process::id(), socket = %socket.display(), "daemon started");
    let uid = rustix::process::getuid().as_raw();

    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    let mut hup = signal(SignalKind::hangup())?;
    let mut stopping = daemon.shutdown.subscribe();
    let mut goodbye = daemon.goodbye.subscribe();
    loop {
        let upgrading = daemon.upgrading.load(std::sync::atomic::Ordering::SeqCst);
        tokio::select! {
            // While handing off, new clients wait in the backlog for the next image.
            _ = goodbye.changed() => {}
            accepted = listener.accept(), if !upgrading => {
                let Ok((stream, _)) = accepted else { continue };
                match stream.peer_cred() {
                    Ok(cred) if cred.uid() == uid => {
                        let pid = cred.pid().and_then(|p| u32::try_from(p).ok());
                        tokio::spawn(conn::serve(Arc::clone(&daemon), stream, pid));
                    }
                    Ok(cred) => tracing::warn!(peer_uid = cred.uid(), "rejected connection from another user"),
                    Err(e) => tracing::warn!("peer credentials unavailable: {e}"),
                }
            }
            _ = term.recv() => { let _ = daemon.shutdown.send(true); }
            _ = int.recv() => { let _ = daemon.shutdown.send(true); }
            // No controlling terminal to lose; ignore so a stray HUP can't take sessions down.
            _ = hup.recv() => {}
            () = async { let _ = stopping.wait_for(|s| *s).await; } => break,
        }
    }
    tracing::info!("shutting down");
    daemon.serve_child.kill();
    drop(listener);
    remove_if_ours(&socket, socket_id);
    // SIGTERM means `stop --hibernate` (SPEC §4.4); until hibernation exists (M4), stop gracefully.
    daemon.stop_all().await;
    if let Some(store) = &daemon.store {
        let _ = store.flush().await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    tracing::info!("bye");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let second = bind(&path).unwrap();
        let ours = file_id(&path);
        // Another daemon replaced the file meanwhile: leave its socket alone.
        drop(second);
        std::fs::remove_file(&path).unwrap();
        let third = bind(&path).unwrap();
        remove_if_ours(&path, ours);
        assert!(path.exists(), "not ours any more");
        remove_if_ours(&path, file_id(&path));
        assert!(!path.exists());
        drop(third);
    }
}
