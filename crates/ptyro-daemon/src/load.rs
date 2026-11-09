//! The computer's load average for the clients' status bars (`[ui] load`, SPEC §11.1): sampled
//! every 5 s (the kernel's own interval), sent as `HostLoad` when it changes, `None` once off.

use std::sync::Arc;
use std::time::Duration;

use ptyro_core::sync::MutexExt;
use ptyro_protocol::{DomainEvent, HostLoad};

use crate::daemon::Daemon;

/// The load now, in hundredths, if the system keeps one.
pub(crate) fn sample() -> Option<HostLoad> {
    let avg = ptyro_pty::load_average()?;
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    Some(HostLoad { avg: avg.map(|a| (a.max(0.0) * 100.0).round() as u32), cpus })
}

impl Daemon {
    pub(crate) fn start_host_load(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let now = if me.config().ui.load { sample() } else { None };
                let changed = {
                    let mut last = me.host_load.locked();
                    let changed = *last != now;
                    *last = now;
                    changed
                };
                if changed {
                    let _ = me.events.send(DomainEvent::HostLoad { load: now });
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }
}
