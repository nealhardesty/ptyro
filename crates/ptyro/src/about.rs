//! `ptyro about` (UX2.1): which ptyro this is and where to take a problem. The version and
//! protocol, the repository, the licence, the build, where the config, data, logs and socket
//! live, whether the daemon and the web app run (and whether the daemon is this version), the
//! tools ptyro leans on, and the whole thing as one block of text to paste into a bug report.
//!
//! The TUI runs `ptyro about --json` and the web app's `about` host operation returns the same
//! document, so the three say the same thing. Most of it is the doctor's own data.

use std::path::PathBuf;

use ptyro_core::Error;
use ptyro_core::config::Config;
use ptyro_protocol::{PROTO, Reply, Request};
use serde::Serialize;

use crate::cli::{Ctx, age};
use crate::doctor::Report;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
pub const LICENSE: &str = env!("CARGO_PKG_LICENSE");

/// Where to report a problem.
pub fn issues_url() -> String {
    format!("{}/issues", REPOSITORY.trim_end_matches('/'))
}

#[derive(Debug, Clone, Serialize)]
pub struct Build {
    pub commit: Option<String>,
    pub date: Option<String>,
    pub target: String,
}

pub fn build() -> Build {
    let some = |s: &str| (!s.is_empty()).then(|| s.to_owned());
    Build {
        commit: some(env!("PTYRO_BUILD_COMMIT")),
        date: some(env!("PTYRO_BUILD_DATE")),
        target: env!("PTYRO_BUILD_TARGET").to_owned(),
    }
}

/// A place on disk, to open or copy.
#[derive(Debug, Clone, Serialize)]
pub struct Place {
    pub name: String,
    pub path: PathBuf,
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DaemonAbout {
    pub running: bool,
    pub version: Option<String>,
    pub protocol: Option<String>,
    pub pid: Option<u32>,
    pub started_ms: Option<u64>,
    /// Running, and the same version and protocol as this ptyro.
    pub same: Option<bool>,
    /// What to do about a different version.
    pub fix: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebAbout {
    pub wanted: bool,
    pub up: bool,
    pub url: Option<String>,
    pub text: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,
    /// `git`, `gh` or an agent.
    pub kind: String,
    pub found: bool,
    /// Its version, or where it was found.
    pub text: String,
}

/// One account's limits for one agent, as the daemon last heard them (M4.17 meters).
#[derive(Debug, Clone, Serialize)]
pub struct UsageAbout {
    /// `claude@work`.
    pub label: String,
    pub text: String,
    pub limited: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct About {
    pub version: String,
    pub protocol: String,
    pub repository: String,
    pub issues: String,
    pub license: String,
    pub build: Build,
    pub exe: Option<PathBuf>,
    /// `macos aarch64`.
    pub system: String,
    pub places: Vec<Place>,
    pub daemon: DaemonAbout,
    pub web: Option<WebAbout>,
    pub tools: Vec<Tool>,
    /// Every agent's known usage limits, per profile (empty: none reported yet, or no daemon).
    pub usage: Vec<UsageAbout>,
    /// All of the above as plain text: what to paste into a bug report.
    pub report: String,
}

fn place(name: &str, path: PathBuf) -> Place {
    Place { name: name.into(), exists: path.exists(), path }
}

/// What this ptyro knows, and what the daemon says about itself.
pub async fn collect(ctx: &Ctx) -> About {
    let config_path = Config::path(&ctx.paths);
    let config = Config::load(&config_path).unwrap_or_default();
    let proto = format!("{}.{}", PROTO.major, PROTO.minor);
    let mut daemon = DaemonAbout {
        running: false,
        version: None,
        protocol: None,
        pid: None,
        started_ms: None,
        same: None,
        fix: None,
    };
    let mut web = None;
    let mut usage = Vec::new();
    if let Ok(c) = ctx.connect(false).await
        && let Ok(Reply::Status { status }) = c.request(Request::Status).await
    {
        if let Ok(Reply::Usage { usage: mut all }) = c.request(Request::Usage).await {
            all.sort_by(|a, b| (&a.agent, &a.profile).cmp(&(&b.agent, &b.profile)));
            let now = ptyro_core::now_ms();
            usage = all
                .iter()
                .map(|u| UsageAbout {
                    label: format!("{}@{}", u.agent, u.profile),
                    text: u.summary(now),
                    limited: u.limited_until_ms.is_some_and(|t| t > now),
                })
                .collect();
        }
        let same = status.version == VERSION && status.proto == PROTO;
        daemon = DaemonAbout {
            running: true,
            version: Some(status.version.clone()),
            protocol: Some(format!("{}.{}", status.proto.major, status.proto.minor)),
            pid: Some(status.pid),
            started_ms: Some(status.started_ms),
            same: Some(same),
            fix: (!same).then(|| {
                "run `ptyro daemon upgrade` (sessions keep running); the daemon keeps the old \
                 version until then"
                    .to_owned()
            }),
        };
        web = status.web.map(|w| WebAbout {
            wanted: w.wanted,
            up: w.up,
            url: w.url.clone(),
            text: crate::webapp::lines(&w),
        });
    }
    // The doctor's own checks for git and the agents.
    let mut r = Report::default();
    crate::doctor::git(&mut r).await;
    crate::doctor::agents(&config, &mut r).await;
    let mut tools: Vec<Tool> =
        r.0.iter()
            .filter(|c| c.section == "git" && c.name == "version" || c.section == "agents")
            .map(|c| Tool {
                name: if c.section == "git" { "git".into() } else { c.name.clone() },
                kind: if c.section == "git" { "git".into() } else { "agent".into() },
                found: c.level != crate::doctor::Level::Problem,
                text: c.text.clone(),
            })
            .collect();
    let gh = crate::doctor::which("gh");
    tools.insert(
        usize::from(!tools.is_empty()),
        Tool {
            name: "gh".into(),
            kind: "gh".into(),
            found: gh.is_some(),
            text: gh.map_or_else(
                || "not found (pull request features need it)".into(),
                |p| p.display().to_string(),
            ),
        },
    );
    let places = vec![
        place("config", config_path),
        place("data", ctx.paths.data.clone()),
        place("logs", ctx.paths.log_dir()),
        place("socket", ctx.paths.socket.clone()),
    ];
    let mut about = About {
        version: VERSION.into(),
        protocol: proto,
        repository: REPOSITORY.into(),
        issues: issues_url(),
        license: LICENSE.into(),
        build: build(),
        exe: std::env::current_exe().ok(),
        system: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        places,
        daemon,
        web,
        tools,
        usage,
        report: String::new(),
    };
    about.report = about.text();
    about
}

impl About {
    /// The whole thing as text, one fact per line, for a terminal and for pasting.
    pub fn text(&self) -> String {
        let mut o = String::new();
        let mut line = |k: &str, v: String| o.push_str(&format!("{k:<12} {v}\n"));
        line("ptyro", format!("{} (protocol {})", self.version, self.protocol));
        let b = &self.build;
        line(
            "build",
            [
                b.commit.as_ref().map(|c| format!("commit {c}")),
                b.date.as_ref().map(|d| format!("of {d}")),
                Some(b.target.clone()).filter(|t| !t.is_empty()),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" "),
        );
        if let Some(e) = &self.exe {
            line("binary", e.display().to_string());
        }
        line("system", self.system.clone());
        line("source", self.repository.clone());
        line("licence", self.license.clone());
        line("report", format!("{} (paste this block)", self.issues));
        let d = &self.daemon;
        line(
            "daemon",
            match (&d.version, &d.protocol, d.pid) {
                (Some(v), Some(p), Some(pid)) => format!(
                    "running, version {v} (protocol {p}), pid {pid}, up {}{}",
                    d.started_ms.map(age).unwrap_or_default(),
                    if d.same == Some(false) { ", NOT this version" } else { "" }
                ),
                _ => "not running (it starts on demand)".into(),
            },
        );
        if let Some(fix) = &d.fix {
            line("", format!("-> {fix}"));
        }
        if let Some(w) = &self.web {
            line("web app", w.text.first().cloned().unwrap_or_default());
        }
        for p in &self.places {
            line(
                &p.name,
                format!("{}{}", p.path.display(), if p.exists { "" } else { " (not made yet)" }),
            );
        }
        for t in &self.tools {
            line(&t.name, t.text.clone());
        }
        for u in &self.usage {
            line(&u.label, u.text.clone());
        }
        o
    }
}

pub async fn run(ctx: &Ctx, json: bool) -> Result<i32, Error> {
    let about = collect(ctx).await;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&about).map_err(|e| Error::internal(e.to_string()))?
        );
    } else {
        print!("{}", about.report);
    }
    Ok(0)
}
