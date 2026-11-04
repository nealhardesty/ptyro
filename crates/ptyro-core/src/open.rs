//! Open an address or a folder with what this computer uses for it (WIN2): inside WSL, Windows
//! does it (Explorer hands an address to the Windows browser and shows a folder by its Windows
//! path); macOS has `open`; other Unix `xdg-open`. WSL rarely has xdg-open.

use std::path::Path;

/// The program and its argument for `what` (an address, or a folder's path).
pub fn argv(what: &Opened<'_>, wsl: Option<&crate::wsl::Wsl>) -> (String, String) {
    let program = match wsl {
        Some(w) => w.windows_exe("explorer.exe").to_string_lossy().into_owned(),
        None if cfg!(target_os = "macos") => "open".into(),
        None => "xdg-open".into(),
    };
    let arg = match what {
        Opened::Url(u) => (*u).to_owned(),
        Opened::Folder(dir) => wsl
            .and_then(|w| w.windows_path(dir))
            .unwrap_or_else(|| dir.to_string_lossy().into_owned()),
    };
    (program, arg)
}

pub enum Opened<'a> {
    Url(&'a str),
    Folder(&'a Path),
}

/// Open it, without waiting. `false`: nothing here opens it, so the caller shows or copies it.
pub fn open(what: &Opened<'_>) -> bool {
    let wsl = crate::wsl::detect().filter(|w| w.interop);
    let (program, arg) = argv(what, wsl);
    std::process::Command::new(program)
        .arg(arg)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_opens_through_explorer_with_windows_paths() {
        let w = crate::wsl::from_facts(
            "6.6.87.2-microsoft-standard-WSL2",
            Some("Ubuntu"),
            true,
            "",
            "",
        )
        .unwrap();
        let (p, a) = argv(&Opened::Url("http://127.0.0.1:7777/#pair=abc"), Some(&w));
        assert!(p.ends_with("explorer.exe"), "{p}");
        assert_eq!(a, "http://127.0.0.1:7777/#pair=abc");
        let (_, a) = argv(&Opened::Folder(Path::new("/home/me/dev")), Some(&w));
        assert_eq!(a, r"\\wsl.localhost\Ubuntu\home\me\dev");
        let (p, a) = argv(&Opened::Folder(Path::new("/home/me")), None);
        assert_eq!(p, if cfg!(target_os = "macos") { "open" } else { "xdg-open" });
        assert_eq!(a, "/home/me");
    }
}
