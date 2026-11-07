//! The version table (ROADMAP BOOT0, SPEC §24.1): per agent and tool, and per platform, how it is
//! installed and updated, how to read its installed version, where its newest version comes
//! from, what it needs first, and how its login is started and confirmed. The data is
//! `catalog.toml` beside this file, embedded at build time and checked by the tests; every value
//! in it is dated and marked `[V]`/`[S]`/`[U]` (RESEARCH segment G). Nothing here touches the network:
//! [`Latest::url`] names what to fetch and [`Latest::parse`] reads what came back.

use std::cmp::Ordering;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const DATA: &str = include_str!("catalog.toml");

/// Where something is installed. WSL is a platform of its own: its Linux side is set up apart
/// from Windows' (BOOT8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    Linux,
    Macos,
    Wsl,
    WindowsX64,
    WindowsArm64,
}

impl Platform {
    pub const ALL: [Platform; 5] =
        [Self::Linux, Self::Macos, Self::Wsl, Self::WindowsX64, Self::WindowsArm64];

    /// The platform this process runs on.
    pub fn current() -> Self {
        if cfg!(windows) {
            if cfg!(target_arch = "aarch64") { Self::WindowsArm64 } else { Self::WindowsX64 }
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else if ptyro_core::wsl::detect().is_some() {
            Self::Wsl
        } else {
            Self::Linux
        }
    }

    /// `windows-x64`, as the table names it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Wsl => "wsl",
            Self::WindowsX64 => "windows-x64",
            Self::WindowsArm64 => "windows-arm64",
        }
    }

    pub fn is_windows(self) -> bool {
        matches!(self, Self::WindowsX64 | Self::WindowsArm64)
    }
}

/// A processor architecture, for downloads that name one (Antigravity's manifest, nodejs.org).
/// Windows platforms carry theirs; elsewhere it is the host's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X64,
    Arm64,
}

impl Arch {
    pub fn host() -> Self {
        if cfg!(target_arch = "aarch64") { Self::Arm64 } else { Self::X64 }
    }

    /// `platform`'s own architecture when it has one, else `self`.
    fn on(self, platform: Platform) -> Self {
        match platform {
            Platform::WindowsX64 => Self::X64,
            Platform::WindowsArm64 => Self::Arm64,
            _ => self,
        }
    }
}

/// How sure the table is of a value (RESEARCH's marks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Mark {
    /// Read from the vendor's current documentation, or seen from the program itself.
    #[serde(rename = "V")]
    Verified,
    /// Second-hand: the vendor's installer script, or a third-party write-up.
    #[serde(rename = "S")]
    Secondhand,
    /// Nothing to read it from: the value's note says what is missing.
    #[serde(rename = "U")]
    Unverified,
}

impl Mark {
    /// `V`, `S` or `U`.
    pub fn letter(self) -> char {
        match self {
            Self::Verified => 'V',
            Self::Secondhand => 'S',
            Self::Unverified => 'U',
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Agent,
    Tool,
}

/// How a method installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Via {
    /// The vendor's own installer (Claude's native installer, Codex's, Antigravity's…).
    Vendor,
    Npm,
    Brew,
    Winget,
    Apt,
    Dnf,
    /// Apple's Command Line Tools (`xcode-select --install`).
    Xcode,
    /// The official nodejs.org build, unpacked into a folder ptyro owns ([`Method::url`]).
    NodeDist,
}

impl Via {
    /// `node-dist`, as the table names it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Vendor => "vendor",
            Self::Npm => "npm",
            Self::Brew => "brew",
            Self::Winget => "winget",
            Self::Apt => "apt",
            Self::Dnf => "dnf",
            Self::Xcode => "xcode",
            Self::NodeDist => "node-dist",
        }
    }

    /// For people: `npm`, `Homebrew`, `the vendor's installer`.
    pub fn words(self) -> &'static str {
        match self {
            Self::Vendor => "the vendor's installer",
            Self::Npm => "npm",
            Self::Brew => "Homebrew",
            Self::Winget => "WinGet",
            Self::Apt => "apt",
            Self::Dnf => "dnf",
            Self::Xcode => "Xcode's Command Line Tools",
            Self::NodeDist => "nodejs.org's build",
        }
    }
}

/// What runs a method's command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Shell {
    /// `sh -c` (the command may pipe into `bash`).
    Sh,
    /// `powershell -NoProfile -ExecutionPolicy Bypass -Command`.
    Powershell,
}

impl Shell {
    /// The argv that runs `command` with this shell.
    pub fn argv(self, command: &str) -> Vec<String> {
        let pre: &[&str] = match self {
            Self::Sh => &["sh", "-c"],
            Self::Powershell => {
                &["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"]
            }
        };
        pre.iter().map(|s| (*s).to_owned()).chain([command.to_owned()]).collect()
    }
}

/// One way to install an item on some platforms.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Method {
    pub platforms: Vec<Platform>,
    pub via: Via,
    /// `None` only for [`Via::NodeDist`], which downloads [`Method::url`].
    #[serde(default)]
    pub shell: Option<Shell>,
    /// A program that must be on `PATH` for this method (`apt-get`, `brew`, `winget`, `npm`).
    #[serde(default)]
    pub needs: Option<String>,
    #[serde(default)]
    pub install: Option<String>,
    /// `None`: the vendor documents no update command for this method.
    #[serde(default)]
    pub update: Option<String>,
    /// The item updates itself in the background when installed this way.
    #[serde(default)]
    pub auto_update: bool,
    /// The method asks for administrator rights (sudo, a UAC prompt).
    #[serde(default)]
    pub admin: bool,
    /// [`Via::NodeDist`]: the download, with `{version}`, `{os}`, `{arch}` and `{ext}`.
    #[serde(default)]
    pub url: Option<String>,
    /// [`Via::NodeDist`]: the checksums file, with `{version}`.
    #[serde(default)]
    pub sums: Option<String>,
    pub mark: Mark,
    #[serde(default)]
    pub note: Option<String>,
}

impl Method {
    pub fn on(&self, platform: Platform) -> bool {
        self.platforms.contains(&platform)
    }

    /// [`Via::NodeDist`]: the build of Node.js `version` (`26.13.0`) for `platform`.
    pub fn download(&self, version: &str, platform: Platform, arch: Arch) -> Option<String> {
        let os = match platform {
            Platform::Linux | Platform::Wsl => "linux",
            Platform::Macos => "darwin",
            Platform::WindowsX64 | Platform::WindowsArm64 => "win",
        };
        let arch = match arch.on(platform) {
            Arch::X64 => "x64",
            Arch::Arm64 => "arm64",
        };
        let ext = if platform.is_windows() { "zip" } else { "tar.xz" };
        let v = version.trim_start_matches('v');
        Some(
            self.url
                .as_deref()?
                .replace("{version}", v)
                .replace("{os}", os)
                .replace("{arch}", arch)
                .replace("{ext}", ext),
        )
    }

    /// [`Via::NodeDist`]: the checksums file for Node.js `version`.
    pub fn sums_url(&self, version: &str) -> Option<String> {
        Some(self.sums.as_deref()?.replace("{version}", version.trim_start_matches('v')))
    }
}

/// Something an item needs first.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prereq {
    /// The needed item's id (`node`, `git`).
    pub item: String,
    /// Its minimum version, when the vendor names one.
    #[serde(default)]
    pub min: Option<String>,
    /// `false`: recommended, not needed (the item runs without it).
    pub required: bool,
    /// Only on these platforms; empty means all.
    #[serde(default)]
    pub platforms: Vec<Platform>,
    pub why: String,
}

/// How an item's login is started and confirmed (BOOT4, [`crate::setup::login`]). ptyro runs
/// `start` where the person sees it, and decides "logged in" by `check`'s exit code alone, its
/// output thrown away: it never reads, parses or stores a credential.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Login {
    pub start: Vec<String>,
    /// Exits 0 when logged in. `None`: the vendor documents no way to confirm a login.
    #[serde(default)]
    pub check: Option<Vec<String>>,
    pub mark: Mark,
    #[serde(default)]
    pub note: Option<String>,
}

/// The shape of a newest-version source's reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// The npm registry's `/<package>/latest`: `{"version": …}`.
    Npm,
    /// A GitHub release (`/releases/latest`): `{"tag_name": …}`, less [`Latest::tag_prefix`].
    GithubRelease,
    /// A body that is just the version (Claude's `latest`).
    PlainText,
    /// Antigravity's per-platform manifest: `{"version": …, "url": …, "sha512": …}`.
    AgyManifest,
    /// nodejs.org's `dist/index.json`: the newest release whose `lts` isn't `false`.
    NodeDist,
}

/// Where an item's newest version comes from.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Latest {
    pub source: Source,
    /// May hold `{platform}` (Antigravity's manifest: `linux_amd64`, `windows_arm64`…).
    url: String,
    #[serde(default)]
    pub tag_prefix: Option<String>,
    pub mark: Mark,
    #[serde(default)]
    pub note: Option<String>,
}

/// A reply that isn't what its source sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadReply(pub String);

impl std::fmt::Display for BadReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BadReply {}

impl Latest {
    /// A source that isn't a row of the table: ptyro's own releases (BOOT6, [`crate::own`]).
    pub fn new(source: Source, url: &str, tag_prefix: Option<&str>, mark: Mark) -> Self {
        Self {
            source,
            url: url.to_owned(),
            tag_prefix: tag_prefix.map(str::to_owned),
            mark,
            note: None,
        }
    }

    /// What to fetch for `platform` (and, off Windows, `arch`).
    pub fn url(&self, platform: Platform, arch: Arch) -> String {
        let os = match platform {
            Platform::Linux | Platform::Wsl => "linux",
            Platform::Macos => "darwin",
            Platform::WindowsX64 | Platform::WindowsArm64 => "windows",
        };
        let arch = match arch.on(platform) {
            Arch::X64 => "amd64",
            Arch::Arm64 => "arm64",
        };
        self.url.replace("{platform}", &format!("{os}_{arch}"))
    }

    /// The newest version in `body`, the reply from [`Latest::url`].
    pub fn parse(&self, body: &str) -> Result<String, BadReply> {
        let bad = |why: &str| BadReply(format!("{:?} reply: {why}", self.source));
        let json =
            || serde_json::from_str::<serde_json::Value>(body).map_err(|e| bad(&e.to_string()));
        let field = |v: &serde_json::Value, k: &str| -> Result<String, BadReply> {
            v.get(k)
                .and_then(|x| x.as_str())
                .map(str::to_owned)
                .ok_or_else(|| bad(&format!("no {k}")))
        };
        let v = match self.source {
            Source::Npm | Source::AgyManifest => field(&json()?, "version")?,
            Source::GithubRelease => {
                let tag = field(&json()?, "tag_name")?;
                let p = self.tag_prefix.as_deref().unwrap_or("");
                tag.strip_prefix(p).ok_or_else(|| bad(&format!("tag {tag} lacks {p}")))?.to_owned()
            }
            Source::PlainText => body.trim().to_owned(),
            Source::NodeDist => {
                let all = json()?;
                let lts = all
                    .as_array()
                    .ok_or_else(|| bad("not a list"))?
                    .iter()
                    .find(|r| r.get("lts").is_some_and(|l| l.as_bool() != Some(false)))
                    .ok_or_else(|| bad("no LTS release"))?;
                field(lts, "version")?.trim_start_matches('v').to_owned()
            }
        };
        match Version::parse(&v) {
            Some(_) if !v.contains(char::is_whitespace) => Ok(v),
            _ => Err(bad(&format!("{v:?} isn't a version"))),
        }
    }
}

/// One agent or tool.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    /// `claude`, `antigravity`, `gh`, `node`: the agent's built-in name, or the tool's.
    pub id: String,
    /// For people: `Claude Code`.
    pub name: String,
    pub kind: Kind,
    /// The command on `PATH`: `claude`, `agy`.
    pub program: String,
    /// A qualifier shown beside the name (Gemini CLI's "for Gemini API-key…").
    #[serde(default)]
    pub label: Option<String>,
    /// What it is, in a line for people (the setup checklist's description, BOOT2).
    #[serde(default)]
    pub about: String,
    /// The vendor's documentation, where every value was checked.
    pub doc: String,
    /// When (`2026-11-06`).
    pub checked: String,
    /// Prints the installed version.
    pub version: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub login: Option<Login>,
    /// `None`: not checked for new versions.
    #[serde(default)]
    pub latest: Option<Latest>,
    #[serde(default)]
    pub prereqs: Vec<Prereq>,
    /// In order of preference.
    pub methods: Vec<Method>,
    /// Other ways it may already be installed, and how each updates.
    #[serde(default)]
    pub others: Vec<Method>,
}

impl Item {
    /// The ways to install it on `platform`, most preferred first.
    pub fn methods(&self, platform: Platform) -> impl Iterator<Item = &Method> {
        self.methods.iter().filter(move |m| m.on(platform))
    }

    /// The preferred way on `platform` whose needed program `has` says is there.
    pub fn method(&self, platform: Platform, has: impl Fn(&str) -> bool) -> Option<&Method> {
        self.methods(platform).find(|m| m.needs.as_deref().is_none_or(&has))
    }

    /// What it needs first on `platform`.
    pub fn prereqs(&self, platform: Platform) -> impl Iterator<Item = &Prereq> {
        self.prereqs
            .iter()
            .filter(move |p| p.platforms.is_empty() || p.platforms.contains(&platform))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    item: Vec<Item>,
}

/// Every item, agents first, in the table's order.
/// A table that doesn't load is empty: every lookup finds nothing and the rest of ptyro works
/// (the tests load it, so a release never ships one).
pub fn items() -> &'static [Item] {
    static ITEMS: OnceLock<Vec<Item>> = OnceLock::new();
    ITEMS.get_or_init(|| load(DATA).unwrap_or_default())
}

/// The rows in `text`, a table in `catalog.toml`'s form.
pub fn load(text: &str) -> Result<Vec<Item>, toml::de::Error> {
    toml::from_str::<File>(text).map(|f| f.item)
}

/// The item `id` (`claude`, `gh`).
pub fn item(id: &str) -> Option<&'static Item> {
    items().iter().find(|i| i.id == id)
}

/// The item whose command is `program` (`agy` → Antigravity).
pub fn by_program(program: &str) -> Option<&'static Item> {
    items().iter().find(|i| i.program == program)
}

/// A version number: `2.1.319`, `0.164.0-alpha.7`. Ordered numerically, a pre-release before
/// its release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub parts: Vec<u64>,
    pub pre: Option<String>,
}

impl Version {
    /// The first version-looking word in `text`: `2.1.319 (Claude Code)`, `codex-cli 0.162.1`,
    /// `v24.13.0`, `gh version 2.57.0 (2024-09-16)`.
    pub fn parse(text: &str) -> Option<Self> {
        text.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',').find_map(|w| {
            let w = w.strip_prefix('v').unwrap_or(w);
            let (num, pre) = match w.split_once('-') {
                Some((n, p)) => (n, Some(p.to_owned())),
                None => (w, None),
            };
            // Up to the first part that isn't a number: Git for Windows says `2.51.0.windows.1`.
            let parts: Vec<u64> = num.split('.').map_while(|p| p.parse().ok()).collect();
            let whole = parts.len() == num.split('.').count();
            (parts.len() >= 2).then_some(Self { parts, pre: if whole { pre } else { None } })
        })
    }

    /// At least `min` (`20.0.0`, `2.23`).
    pub fn at_least(&self, min: &str) -> bool {
        Self::parse(min).is_some_and(|m| *self >= m)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let n = self.parts.len().max(other.parts.len());
        let at = |v: &Self, i: usize| v.parts.get(i).copied().unwrap_or(0);
        (0..n).map(|i| at(self, i).cmp(&at(other, i))).find(|o| o.is_ne()).unwrap_or_else(
            || match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            },
        )
    }
}

impl std::fmt::Display for Version {
    /// `2.1.319`, `0.164.0-alpha.7`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let nums: Vec<String> = self.parts.iter().map(u64::to_string).collect();
        f.write_str(&nums.join("."))?;
        match &self.pre {
            Some(p) => write!(f, "-{p}"),
            None => Ok(()),
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/catalog")
            .join(name);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    #[test]
    fn the_table_loads_and_has_every_row() {
        if let Err(e) = load(DATA) {
            panic!("catalog.toml doesn't load: {e}");
        }
        assert!(load("[[item]]\nid = \"x\"\n").is_err(), "a row missing its fields is an error");
        let ids: Vec<&str> = items().iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            ["claude", "codex", "antigravity", "opencode", "gemini", "git", "gh", "node"]
        );
        // Every built-in agent has its row, under its built-in name and program.
        for (name, prog) in crate::BUILTIN {
            let i = item(name).unwrap_or_else(|| panic!("{name} has no row"));
            assert_eq!(i.program, *prog);
            assert_eq!(i.kind, Kind::Agent);
        }
        assert_eq!(by_program("agy").map(|i| i.id.as_str()), Some("antigravity"));
        assert!(item("gemini").unwrap().label.as_deref().unwrap().contains("Antigravity"));
    }

    #[test]
    fn every_row_is_complete_on_every_platform() {
        for i in items() {
            assert!(i.doc.starts_with("https://"), "{}: doc", i.id);
            assert!(!i.about.is_empty(), "{}: the checklist's line, `about`", i.id);
            let d: Vec<&str> = i.checked.split('-').collect();
            assert!(
                d.len() == 3 && d.iter().all(|p| p.parse::<u32>().is_ok()) && d[0].len() == 4,
                "{}: checked {:?} isn't a date",
                i.id,
                i.checked
            );
            assert_eq!(i.version.first(), Some(&i.program), "{}: version runs the program", i.id);
            for p in Platform::ALL {
                // A platform's last method needs nothing beyond itself, or is a package manager
                // the platform has (WinGet on Windows, Homebrew…): there is always one to offer.
                assert!(i.methods(p).next().is_some(), "{} has no method on {}", i.id, p.name());
            }
            for m in i.methods.iter().chain(&i.others) {
                assert!(!m.platforms.is_empty(), "{}: a method on no platform", i.id);
                if m.via == Via::NodeDist {
                    assert!(m.url.is_some() && m.sums.is_some() && m.install.is_none(), "{}", i.id);
                } else {
                    let cmd =
                        m.install.as_deref().unwrap_or_else(|| panic!("{}: no install", i.id));
                    assert!(!cmd.trim().is_empty(), "{}", i.id);
                    let sh = m.shell.unwrap_or_else(|| panic!("{}: {cmd} has no shell", i.id));
                    // PowerShell on Windows, sh everywhere else.
                    for p in &m.platforms {
                        assert_eq!(
                            p.is_windows(),
                            sh == Shell::Powershell,
                            "{}: {cmd} on {}",
                            i.id,
                            p.name()
                        );
                    }
                    // WinGet runs with no input: its first-use agreements are accepted up
                    // front, and its own source named (not the msstore's).
                    for c in [Some(cmd), m.update.as_deref()].into_iter().flatten() {
                        if m.via == Via::Winget {
                            for flag in [
                                "--source winget",
                                "--accept-source-agreements",
                                "--accept-package-agreements",
                            ] {
                                assert!(c.contains(flag), "{}: {c} lacks {flag}", i.id);
                            }
                        }
                    }
                    // sudo means admin, and admin is said.
                    if cmd.contains("sudo ") {
                        assert!(m.admin, "{}: {cmd} uses sudo but isn't admin", i.id);
                    }
                }
                if m.mark != Mark::Verified {
                    assert!(m.note.is_some(), "{}: a {:?} method says why", i.id, m.mark);
                }
            }
            for p in &i.prereqs {
                assert!(item(&p.item).is_some(), "{} needs {}, which has no row", i.id, p.item);
                if let Some(min) = &p.min {
                    assert!(Version::parse(min).is_some(), "{}: min {min}", i.id);
                }
            }
            if let Some(l) = &i.login {
                assert!(!l.start.is_empty(), "{}", i.id);
                if l.check.is_none() || l.mark != Mark::Verified {
                    assert!(l.note.is_some(), "{}: an unconfirmed login says why", i.id);
                }
            }
            if let Some(l) = &i.latest
                && l.mark != Mark::Verified
            {
                assert!(l.note.is_some(), "{}: a {:?} source says why", i.id, l.mark);
            }
        }
    }

    #[test]
    fn agents_need_no_admin_and_are_checked_for_updates() {
        for i in items().iter().filter(|i| i.kind == Kind::Agent) {
            assert!(i.latest.is_some(), "{} has no newest-version source", i.id);
            assert!(i.login.is_some(), "{} has no login", i.id);
            for m in &i.methods {
                assert!(!m.admin, "{}: the recommended way needs admin", i.id);
                assert!(m.update.is_some() || m.auto_update, "{}: no way to update", i.id);
            }
        }
        // The design's recommended methods (RESEARCH G, 2026-11-06).
        let lin = |id: &str| item(id).unwrap().method(Platform::Linux, |_| true).unwrap();
        let win = |id: &str| item(id).unwrap().method(Platform::WindowsArm64, |_| true).unwrap();
        assert_eq!(
            lin("claude").install.as_deref(),
            Some("curl -fsSL https://claude.ai/install.sh | bash")
        );
        assert_eq!(
            win("claude").install.as_deref(),
            Some("irm https://claude.ai/install.ps1 | iex")
        );
        assert_eq!(lin("codex").via, Via::Vendor);
        assert_eq!(lin("antigravity").via, Via::Vendor);
        assert_eq!(lin("gemini").via, Via::Npm);
        assert_eq!(win("opencode").via, Via::Npm);
        assert_eq!(win("gh").via, Via::Winget);
    }

    #[test]
    fn a_method_whose_program_is_missing_is_passed_over() {
        let node = item("node").unwrap();
        let none = |_: &str| false;
        assert_eq!(node.method(Platform::Linux, |p| p == "dnf").unwrap().via, Via::Dnf);
        assert_eq!(node.method(Platform::Linux, |p| p == "apt-get").unwrap().via, Via::Apt);
        assert_eq!(node.method(Platform::Macos, |p| p == "brew").unwrap().via, Via::Brew);
        assert_eq!(node.method(Platform::Macos, none).unwrap().via, Via::NodeDist);
        assert_eq!(node.method(Platform::WindowsX64, none).unwrap().via, Via::NodeDist);
        assert!(item("gemini").unwrap().method(Platform::Linux, none).is_none());
        let git = item("git").unwrap();
        assert_eq!(git.method(Platform::Macos, none).unwrap().via, Via::Xcode);
    }

    #[test]
    fn prerequisites_by_platform() {
        let g = item("gemini").unwrap();
        let p: Vec<_> = g.prereqs(Platform::Linux).collect();
        assert_eq!(
            (p[0].item.as_str(), p[0].min.as_deref(), p[0].required),
            ("node", Some("20.0.0"), true)
        );
        let oc = item("opencode").unwrap();
        assert_eq!(oc.prereqs(Platform::Linux).count(), 0);
        assert_eq!(oc.prereqs(Platform::WindowsX64).next().unwrap().item, "node");
        assert!(item("claude").unwrap().prereqs(Platform::WindowsArm64).all(|p| !p.required));
    }

    #[test]
    fn newest_versions_from_recorded_replies() {
        let parse =
            |id: &str, f: &str| item(id).unwrap().latest.as_ref().unwrap().parse(&fixture(f));
        assert_eq!(parse("claude", "claude-latest.txt").unwrap(), "2.1.319");
        assert_eq!(parse("codex", "codex-latest.json").unwrap(), "0.167.0");
        assert_eq!(parse("antigravity", "antigravity-latest-linux_amd64.json").unwrap(), "1.5.2");
        assert!(parse("antigravity", "antigravity-latest-windows_arm64.json").is_ok());
        assert_eq!(parse("opencode", "opencode-latest.json").unwrap(), "1.19.5");
        assert_eq!(parse("gemini", "gemini-latest.json").unwrap(), "0.67.0");
        assert_eq!(parse("gh", "gh-latest.json").unwrap(), "2.103.3");
        // The newest LTS, not the newer alpha above it.
        assert!(fixture("node-latest.json").contains("alpha"));
        assert_eq!(parse("node", "node-latest.json").unwrap(), "26.13.0");
        assert!(item("git").unwrap().latest.is_none());
    }

    #[test]
    fn a_bad_reply_is_an_error_not_a_version() {
        let l = |id: &str| item(id).unwrap().latest.clone().unwrap();
        assert!(l("claude").parse("<html>Service Unavailable</html>").is_err());
        assert!(l("claude").parse("").is_err());
        assert!(l("gemini").parse("{\"name\":\"x\"}").is_err());
        assert!(l("gemini").parse("not json").is_err());
        assert!(l("codex").parse("{\"tag_name\":\"v1.2.3\"}").is_err(), "wrong tag prefix");
        assert!(l("node").parse("[{\"version\":\"v27.0.0\",\"lts\":false}]").is_err());
    }

    #[test]
    fn source_urls_per_platform() {
        let agy = item("antigravity").unwrap().latest.clone().unwrap();
        let base =
            "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/";
        assert_eq!(agy.url(Platform::Linux, Arch::X64), format!("{base}linux_amd64.json"));
        assert_eq!(agy.url(Platform::Wsl, Arch::Arm64), format!("{base}linux_arm64.json"));
        assert_eq!(agy.url(Platform::Macos, Arch::Arm64), format!("{base}darwin_arm64.json"));
        // Windows' platforms carry their architecture.
        assert_eq!(agy.url(Platform::WindowsArm64, Arch::X64), format!("{base}windows_arm64.json"));
        let node = item("node").unwrap();
        let dist = node.methods.iter().find(|m| m.via == Via::NodeDist).unwrap();
        assert_eq!(
            dist.download("v26.13.0", Platform::Wsl, Arch::X64).unwrap(),
            "https://nodejs.org/dist/v26.13.0/node-v26.13.0-linux-x64.tar.xz"
        );
        assert_eq!(
            dist.download("26.13.0", Platform::WindowsArm64, Arch::X64).unwrap(),
            "https://nodejs.org/dist/v26.13.0/node-v26.13.0-win-arm64.zip"
        );
        assert_eq!(
            dist.download("26.13.0", Platform::Macos, Arch::Arm64).unwrap(),
            "https://nodejs.org/dist/v26.13.0/node-v26.13.0-darwin-arm64.tar.xz"
        );
        assert_eq!(
            dist.sums_url("26.13.0").unwrap(),
            "https://nodejs.org/dist/v26.13.0/SHASUMS256.txt"
        );
    }

    #[test]
    fn versions_read_and_compare() {
        let v = |s: &str| Version::parse(s).unwrap();
        assert_eq!(v("2.1.319 (Claude Code)").parts, [2, 1, 319]);
        assert_eq!(v("codex-cli 0.162.1").parts, [0, 162, 1]);
        assert_eq!(v("v24.13.0").parts, [24, 13, 0]);
        assert_eq!(v("git version 2.43.0").parts, [2, 43, 0]);
        assert_eq!(v("gh version 2.57.0 (2024-09-16)").parts, [2, 57, 0]);
        assert_eq!(v("0.164.0-alpha.7").pre.as_deref(), Some("alpha.7"));
        assert!(Version::parse("no version here").is_none());
        // What the table's `version` argvs print on each platform.
        assert_eq!(v("git version 2.51.0.windows.1").parts, [2, 51, 0]);
        assert_eq!(v("git version 2.51.0.windows.1").pre, None);
        assert!(v("git version 2.51.0.windows.1").at_least("2.23"));
        assert_eq!(v("git version 2.39.5 (Apple Git-154)").parts, [2, 39, 5]);
        assert_eq!(
            v("gh version 2.83.0 (2025-11-04)\nhttps://github.com/cli/cli/releases/tag/v2.83.0")
                .parts,
            [2, 83, 0]
        );
        assert_eq!(v("v22.11.0").parts, [22, 11, 0]);
        assert_eq!(v("v26.13.0\r\n").parts, [26, 13, 0]);
        assert_eq!(v("0.67.0").parts, [0, 67, 0]);
        assert_eq!(v("1.19.5").parts, [1, 19, 5]);
        assert_eq!(v("codex-cli 0.167.0-alpha.2").pre.as_deref(), Some("alpha.2"));
        assert!(v("2.1.319") > v("2.1.99"));
        assert!(v("0.164.0-alpha.7") < v("0.164.0"));
        assert!(v("0.164.0-alpha.7") > v("0.163.0"));
        assert_eq!(v("2.23").cmp(&v("2.23.0")), Ordering::Equal);
        assert!(v("v24.13.0").at_least("20.0.0"));
        assert!(!v("v18.19.1").at_least("20.0.0"));
        assert!(v("git version 2.43.0").at_least("2.23"));
    }

    #[test]
    fn shells_build_their_argv() {
        assert_eq!(Shell::Sh.argv("echo hi"), ["sh", "-c", "echo hi"]);
        assert_eq!(Shell::Powershell.argv("irm x | iex").last().unwrap(), "irm x | iex");
        assert!(Platform::ALL.contains(&Platform::current()));
    }
}
