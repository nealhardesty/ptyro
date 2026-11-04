//! Shells and ptyro's shell integration (SPEC §5.7, §8.7; ROADMAP WIN1).
//!
//! The integration is a small snippet per shell that reports, over OSC 133, where each prompt and
//! command starts and how the command ended, and over OSC 7 (OSC 9;9 on Windows) the directory.
//! Copy mode's prompt jumps, a shell session's working/idle and its last command, and the session's
//! directory all come from it.
//!
//! `ptyro shell-init <shell>` prints a snippet for an rc file. Shells that ptyro starts itself get
//! it with no setup ([`integrate`], `shell.integration`): ptyro starts the shell so that it reads
//! the user's own startup files first and ptyro's snippet last. Every snippet does nothing outside
//! ptyro (`PTYRO` unset) and nothing twice (`__ptyro_integrated`), so a line in an rc file and the
//! automatic one never stack.

use std::path::{Path, PathBuf};

/// A shell ptyro knows how to integrate with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    Zsh,
    Fish,
    /// PowerShell: `pwsh` (7+, every platform) and Windows PowerShell 5.1 (`powershell.exe`).
    Pwsh,
}

impl ShellKind {
    pub const ALL: [ShellKind; 4] = [Self::Bash, Self::Zsh, Self::Fish, Self::Pwsh];

    /// The name `ptyro shell-init` takes.
    pub fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::Pwsh => "pwsh",
        }
    }

    /// Which shell a program is: `/bin/zsh`, `-zsh` (a login shell's `argv[0]`), `pwsh.exe`,
    /// `pwsh-preview`, `powershell`.
    pub fn of_program(program: &str) -> Option<Self> {
        let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
        let base = base.strip_prefix('-').unwrap_or(base);
        let lower = base.to_ascii_lowercase();
        let base = lower.strip_suffix(".exe").unwrap_or(&lower);
        match base {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            "pwsh" | "pwsh-preview" | "powershell" => Some(Self::Pwsh),
            _ => None,
        }
    }

    /// The snippet for an rc file (what `ptyro shell-init` prints).
    pub fn snippet(self) -> &'static str {
        match self {
            Self::Bash => BASH,
            Self::Zsh => ZSH,
            Self::Fish => FISH,
            Self::Pwsh => PWSH,
        }
    }

    /// The line that loads the snippet from the shell's rc file, and that file (under `home`, or
    /// for fish and PowerShell under `$XDG_CONFIG_HOME` when it is set, as they do).
    /// For PowerShell, the profile of `pwsh` on this platform.
    pub fn rc_line(self, home: &Path) -> (PathBuf, &'static str) {
        match self {
            Self::Bash => (home.join(".bashrc"), "eval \"$(ptyro shell-init bash)\""),
            Self::Zsh => (home.join(".zshrc"), "eval \"$(ptyro shell-init zsh)\""),
            Self::Fish => (
                crate::config::config_home(home).join("fish/config.fish"),
                "ptyro shell-init fish | source",
            ),
            Self::Pwsh => {
                (pwsh_profile(home), "ptyro shell-init pwsh | Out-String | Invoke-Expression")
            }
        }
    }
}

/// `pwsh`'s profile for the current host (`$PROFILE.CurrentUserCurrentHost`).
fn pwsh_profile(home: &Path) -> PathBuf {
    if cfg!(windows) {
        home.join("Documents").join("PowerShell").join("Microsoft.PowerShell_profile.ps1")
    } else {
        crate::config::config_home(home).join("powershell/Microsoft.PowerShell_profile.ps1")
    }
}

/// Is `argv` an interactive shell and nothing else: a known shell with at most login,
/// interactive or no-banner switches (`zsh -l`, `pwsh -NoLogo`)? Such a command is started as a
/// shell session (status from its prompt marks, its integration added), not as a plain command.
pub fn interactive(argv: &[String]) -> Option<ShellKind> {
    let (prog, args) = argv.split_first()?;
    let kind = ShellKind::of_program(prog)?;
    let plain = |a: &String| match kind {
        ShellKind::Pwsh => {
            matches!(
                a.to_ascii_lowercase().as_str(),
                "-l" | "-login" | "-nologo" | "-noprofile" | "-interactive"
            )
        }
        _ => matches!(a.as_str(), "-l" | "--login" | "-i" | "--interactive"),
    };
    args.iter().all(plain).then_some(kind)
}

fn is_login(kind: ShellKind, args: &[String]) -> bool {
    args.iter().any(|a| match kind {
        ShellKind::Pwsh => matches!(a.to_ascii_lowercase().as_str(), "-l" | "-login"),
        _ => a == "-l" || a == "--login",
    })
}

/// The shell to start when nothing names one: on Unix `$SHELL`, else `/bin/sh`. On Windows
/// (RESEARCH DD3.4 item 4) PowerShell 7 (`pwsh.exe`) when it is installed, else Windows
/// PowerShell, else `%COMSPEC%` (cmd.exe).
pub fn default_shell() -> String {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let path = std::env::var_os("PATH").unwrap_or_default();
    default_shell_from(&var, &|prog| std::env::split_paths(&path).any(|d| d.join(prog).is_file()))
}

/// [`default_shell`] with the environment and the `PATH` lookup given.
fn default_shell_from(
    var: &dyn Fn(&str) -> Option<String>,
    on_path: &dyn Fn(&str) -> bool,
) -> String {
    if cfg!(windows) {
        ["pwsh.exe", "powershell.exe"]
            .into_iter()
            .find(|p| on_path(p))
            .map(str::to_owned)
            .or_else(|| var("COMSPEC"))
            .unwrap_or_else(|| "cmd.exe".into())
    } else {
        var("SHELL").unwrap_or_else(|| "/bin/sh".into())
    }
}

/// What a new shell session runs when `[shell] command` is empty: the default shell, as a login
/// shell on Unix (`$SHELL -l`). Windows has no login shells.
pub fn default_shell_argv() -> Vec<String> {
    let shell = default_shell();
    if cfg!(windows) { vec![shell] } else { vec![shell, "-l".into()] }
}

/// How `shell` runs one command line: `-c` for POSIX shells and fish, `-NoProfile -Command` for
/// PowerShell, `/d /s /c` for cmd.exe. (cmd.exe doesn't read the `\"` that Rust's `Command`
/// quotes with, so a line with quotes needs `raw_arg` there: WIN6. Windows 11 always has Windows
/// PowerShell on `PATH`, so cmd.exe is only the last resort of [`default_shell`].)
pub fn command_argv(shell: &str, line: &str) -> Vec<String> {
    let base = shell.rsplit(['/', '\\']).next().unwrap_or(shell).to_ascii_lowercase();
    let flags: &[&str] = if base == "cmd" || base == "cmd.exe" {
        &["/d", "/s", "/c"]
    } else if ShellKind::of_program(shell) == Some(ShellKind::Pwsh) {
        &["-NoProfile", "-Command"]
    } else {
        &["-c"]
    };
    std::iter::once(shell)
        .chain(flags.iter().copied())
        .chain(std::iter::once(line))
        .map(str::to_owned)
        .collect()
}

/// The argv that runs a command line ptyro itself was given to run (a check, a status line's
/// command): `sh -c` on Unix, the [`default_shell`] on Windows.
pub fn command_shell(line: &str) -> Vec<String> {
    if cfg!(windows) { command_argv(&default_shell(), line) } else { command_argv("sh", line) }
}

/// The argv that opens `path` in `editor`, a command line such as `$VISUAL` (`code --wait`,
/// `vim +12`): on Unix `sh -c '<editor> "$1"' sh <path>`, so the path is never parsed by the
/// shell; on Windows the default shell, with the path quoted for it.
pub fn editor_command(editor: &str, path: &Path) -> Vec<std::ffi::OsString> {
    if !cfg!(windows) {
        let line = format!("{editor} \"$1\"");
        let mut argv: Vec<std::ffi::OsString> =
            command_argv("sh", &line).into_iter().map(Into::into).collect();
        argv.extend(["sh".into(), path.as_os_str().to_owned()]);
        return argv;
    }
    editor_command_in(&default_shell(), editor, &path.to_string_lossy())
        .into_iter()
        .map(Into::into)
        .collect()
}

/// [`editor_command`] on Windows, for `shell`.
fn editor_command_in(shell: &str, editor: &str, path: &str) -> Vec<String> {
    let line = if ShellKind::of_program(shell) == Some(ShellKind::Pwsh) {
        // `&` runs a quoted program path too; single quotes are literal, `''` is one quote.
        format!("& {editor} '{}'", path.replace('\'', "''"))
    } else {
        // cmd.exe: a Windows path holds no `"`.
        format!("{editor} \"{path}\"")
    };
    command_argv(shell, &line)
}

/// The variables [`integrate`] may set, cleared from every shell's environment first, so none
/// left over from elsewhere can steer the startup files.
pub const VARS: [&str; 3] = ["PTYRO_BASH_LOGIN", "PTYRO_ZDOTDIR", "PTYRO_ZDOTDIR_UNSET"];

/// How to start an interactive shell so that it reads the user's startup files and then ptyro's
/// integration: the argv to run, variables to set, and files to write first (all under `dir`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Integrated {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub files: Vec<(PathBuf, String)>,
}

/// [`Integrated`] for `argv` (an [`interactive`] shell), with its files under `dir`. `zdotdir` is
/// the session's `ZDOTDIR`, if it has one (zsh only). `None` when `argv` isn't a shell ptyro can
/// integrate with: it then runs as it is.
pub fn integrate(argv: &[String], dir: &Path, zdotdir: Option<&str>) -> Option<Integrated> {
    let kind = interactive(argv)?;
    let (prog, args) = argv.split_first()?;
    let login = is_login(kind, args);
    let snippet = dir.join(format!("ptyro.{}", ext(kind)));
    let mut files = vec![(snippet.clone(), kind.snippet().to_owned())];
    let s = snippet.to_string_lossy().into_owned();
    let mut env = Vec::new();
    let argv = match kind {
        // `--init-file` replaces ~/.bashrc, and a login shell ignores it: so start an interactive
        // shell whose init file does what a login shell would have done (or reads ~/.bashrc), then
        // loads the snippet.
        ShellKind::Bash => {
            let init = dir.join("bash-init");
            files.push((init.clone(), bash_init(&s)));
            if login {
                env.push(("PTYRO_BASH_LOGIN".to_owned(), "1".to_owned()));
            }
            vec![
                prog.clone(),
                "--init-file".into(),
                init.to_string_lossy().into_owned(),
                "-i".into(),
            ]
        }
        // zsh reads its startup files from $ZDOTDIR: point it at ptyro's, which each read the
        // user's own file from where it really is, and the last one loads the snippet.
        ShellKind::Zsh => {
            let zd = dir.join("zsh");
            let ours = zd.to_string_lossy().into_owned();
            for (name, body) in zsh_files(&ours, &s) {
                files.push((zd.join(name), body));
            }
            match zdotdir {
                Some(z) => env.push(("PTYRO_ZDOTDIR".to_owned(), z.to_owned())),
                None => env.push(("PTYRO_ZDOTDIR_UNSET".to_owned(), "1".to_owned())),
            }
            env.push(("ZDOTDIR".to_owned(), ours));
            argv.to_vec()
        }
        // `--init-command` runs after fish's own config files.
        ShellKind::Fish => {
            let mut a = argv.to_vec();
            a.push("--init-command".into());
            a.push(format!("source {}", fish_quote(&s)));
            a
        }
        // `-NoExit -Command` runs after the user's $PROFILE and leaves the shell open; a login
        // switch must stay first (pwsh requires it there).
        ShellKind::Pwsh => {
            let mut a = vec![prog.clone()];
            if login && !cfg!(windows) {
                a.push("-Login".into());
            }
            // The user's own switches (`-NoProfile`), then ptyro's.
            a.extend(
                args.iter()
                    .filter(|x| {
                        !matches!(x.to_ascii_lowercase().as_str(), "-l" | "-login" | "-nologo")
                    })
                    .cloned(),
            );
            a.push("-NoLogo".into());
            a.push("-NoExit".into());
            a.push("-Command".into());
            a.push(format!(". '{}'", s.replace('\'', "''")));
            a
        }
    };
    Some(Integrated { argv, env, files })
}

fn ext(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Bash => "bash",
        ShellKind::Zsh => "zsh",
        ShellKind::Fish => "fish",
        ShellKind::Pwsh => "ps1",
    }
}

/// POSIX single quotes.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn fish_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', r"\\").replace('\'', r"\'"))
}

fn bash_init(snippet: &str) -> String {
    format!(
        r#"# ptyro: bash reads this instead of ~/.bashrc, so that ptyro's shell integration comes last.
if [ -n "$PTYRO_BASH_LOGIN" ]; then
  unset PTYRO_BASH_LOGIN
  # What a login shell reads.
  [ -r /etc/profile ] && . /etc/profile
  for __ptyro_f in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
    if [ -r "$__ptyro_f" ]; then . "$__ptyro_f"; break; fi
  done
  unset __ptyro_f
else
  [ -r "$HOME/.bashrc" ] && . "$HOME/.bashrc"
fi
. {}
"#,
        sh_quote(snippet)
    )
}

/// ptyro's `.zshenv`, `.zprofile`, `.zshrc` and `.zlogin`. Each puts `ZDOTDIR` back to the user's
/// directory while their file runs (their `.zshenv` may move it), then points it at ptyro's again
/// for the next file. `.zshrc` is the last one zsh reads from here: it loads the snippet and leaves
/// `ZDOTDIR` as the user had it (unset, if it was), so `.zlogin` and every child see theirs.
fn zsh_files(ours: &str, snippet: &str) -> Vec<(&'static str, String)> {
    let q = sh_quote(ours);
    let user = |file: &str| {
        format!(
            r#"# ptyro: zsh reads its startup files here, so that ptyro's shell integration comes last.
ZDOTDIR=${{PTYRO_ZDOTDIR:-$HOME}}
[[ -r $ZDOTDIR/{file} ]] && source $ZDOTDIR/{file}
PTYRO_ZDOTDIR=$ZDOTDIR
ZDOTDIR={q}
"#
        )
    };
    let rc = format!(
        r#"# ptyro: zsh reads its startup files here, so that ptyro's shell integration comes last.
ZDOTDIR=${{PTYRO_ZDOTDIR:-$HOME}}
# A system zshrc (macOS's) set the history file while zsh read from here.
[[ $HISTFILE == {q}/* ]] && HISTFILE=$ZDOTDIR/${{HISTFILE:t}}
[[ -r $ZDOTDIR/.zshrc ]] && source $ZDOTDIR/.zshrc
if [[ -n $PTYRO_ZDOTDIR_UNSET && $ZDOTDIR == $HOME ]]; then
  unset ZDOTDIR
fi
unset PTYRO_ZDOTDIR PTYRO_ZDOTDIR_UNSET
source {}
"#,
        sh_quote(snippet)
    );
    // `.zlogin` comes after `.zshrc`, which has already handed ZDOTDIR back: it's read from the
    // user's directory. Ours exists only for a shell whose `.zshrc` stage didn't run (not
    // interactive), so it still finds the user's file.
    vec![
        (".zshenv", user(".zshenv")),
        (".zprofile", user(".zprofile")),
        (".zshrc", rc),
        (".zlogin", user(".zlogin")),
    ]
}

const BASH: &str = r#"# ptyro shell integration for bash. ptyro adds it to the shells it starts; for others, add
# to ~/.bashrc:  eval "$(ptyro shell-init bash)"
if [ -n "$PTYRO" ] && [ -z "$__ptyro_integrated" ]; then
  __ptyro_integrated=1
  __ptyro_prompt() {
    local s=$?
    __ptyro_at_prompt=
    printf '\e]133;D;%s\a\e]7;file://%s%s\a\e]133;A\a' "$s" "$HOSTNAME" "$PWD"
    return $s
  }
  PROMPT_COMMAND="__ptyro_prompt${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
  PS1="$PS1\[\e]133;B\a\]"
  __ptyro_preexec() {
    local c
    c=$(HISTTIMEFORMAT= builtin history 1)
    c="${c#*[0-9] }"; c="${c# }"; c="${c//[[:cntrl:]]/ }"
    printf '\e]133;C;cmdline=%s\a' "${c:0:200}"
  }
  if [ "${BASH_VERSINFO[0]}" -gt 4 ] || { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]}" -ge 4 ]; }; then
    # bash 4.4+: PS0 runs as a command starts.
    PS0='$(__ptyro_preexec)'"$PS0"
  elif [ -z "$(trap -p DEBUG)" ]; then
    # Older bash (macOS's 3.2) has no PS0: a DEBUG trap sees the first command after a prompt.
    # Only when nothing else uses the trap.
    __ptyro_ready() { __ptyro_at_prompt=1; }
    __ptyro_debug() {
      [ -n "$__ptyro_at_prompt" ] || return 0
      case "$BASH_COMMAND" in __ptyro_prompt*|__ptyro_ready*) return 0 ;; esac
      __ptyro_at_prompt=
      __ptyro_preexec
    }
    trap '__ptyro_debug' DEBUG
    PROMPT_COMMAND="$PROMPT_COMMAND;__ptyro_ready"
  fi
fi
"#;

const ZSH: &str = r#"# ptyro shell integration for zsh. ptyro adds it to the shells it starts; for others, add
# to ~/.zshrc:  eval "$(ptyro shell-init zsh)"
if [[ -n $PTYRO && -z $__ptyro_integrated ]]; then
  __ptyro_integrated=1
  autoload -Uz add-zsh-hook
  __ptyro_precmd() {
    local s=$?
    printf '\e]133;D;%s\a\e]7;file://%s%s\a\e]133;A\a' "$s" "$HOST" "$PWD"
  }
  __ptyro_preexec() {
    local c=${1//[[:cntrl:]]/ }
    printf '\e]133;C;cmdline=%s\a' "${c[1,200]}"
  }
  add-zsh-hook precmd __ptyro_precmd
  add-zsh-hook preexec __ptyro_preexec
  PS1="$PS1%{"$'\e]133;B\a'"%}"
fi
"#;

const FISH: &str = r#"# ptyro shell integration for fish. ptyro adds it to the shells it starts; for others, add
# to ~/.config/fish/config.fish:  ptyro shell-init fish | source
if set -q PTYRO; and not set -q __ptyro_integrated
  set -g __ptyro_integrated 1
  function __ptyro_prompt --on-event fish_prompt
    printf '\e]133;D;%s\a\e]7;file://%s%s\a\e]133;A\a' $status (hostname) $PWD
  end
  function __ptyro_preexec --on-event fish_preexec
    printf '\e]133;C;cmdline=%s\a' (string replace -ra '[[:cntrl:]]' ' ' -- "$argv[1]" | string sub -l 200)
  end
end
"#;

/// PowerShell 7+ and Windows PowerShell 5.1 (so no `` `e ``, no `$IsWindows`).
///
/// - The prompt is wrapped, not replaced: oh-my-posh, starship or the user's own `prompt` keep
///   drawing it, and still see `$?` and `$LASTEXITCODE` as the command left them (a failing
///   statement right before the call is the only way to hand `$?` on).
/// - The exit status: 0 when the command succeeded, else a native program's `$LASTEXITCODE`,
///   else 1 (a cmdlet failed: `$Error` got a new entry while `$LASTEXITCODE` stayed as it was).
/// - The command line (133;C) comes from PSReadLine, when it's loaded: the line it returns.
/// - The directory: OSC 7 on macOS and Linux, OSC 9;9 (Windows Terminal's) on Windows, and only
///   when the location is a folder (not `Env:` or `HKLM:`).
const PWSH: &str = r#"# ptyro shell integration for PowerShell. ptyro adds it to the shells it starts; for others,
# add to $PROFILE:  ptyro shell-init pwsh | Out-String | Invoke-Expression
if ($env:PTYRO -and -not $global:__ptyro_integrated) {
  $global:__ptyro_integrated = $true
  $global:__ptyro_prompt = $function:prompt
  function global:prompt {
    $ok = $global:?
    $code = $global:LASTEXITCODE
    $e = [char]27; $a = [char]7
    # A native program's exit code, unless a cmdlet failed since the command started (a new
    # entry in $Error with $LASTEXITCODE unchanged).
    $err = $global:Error.Count -and -not [object]::ReferenceEquals($global:Error[0], $global:__ptyro_err)
    $native = ($code -ne $global:__ptyro_lec) -or -not $err
    if ($ok) { $s = 0 } elseif ($native -and $code -is [int] -and $code -ne 0) { $s = $code } else { $s = 1 }
    $out = "$e]133;D;$s$a"
    $loc = $executionContext.SessionState.Path.CurrentLocation
    if ($loc.Provider.Name -eq 'FileSystem') {
      $p = $loc.ProviderPath -replace '[\x00-\x1f\x7f]', ''
      if ([System.IO.Path]::DirectorySeparatorChar -eq '\') {
        $out += "$e]9;9;`"$p`"$a"
      } else {
        $out += "$e]7;file://$([System.Net.Dns]::GetHostName())$($p -replace '%', '%25')$a"
      }
    }
    $out += "$e]133;A$a"
    $global:LASTEXITCODE = $code
    $shown = $null
    if ($global:__ptyro_prompt) {
      if ($ok) { $shown = & $global:__ptyro_prompt } else { Write-Error '' -ErrorAction Ignore; $shown = & $global:__ptyro_prompt }
    }
    if ($null -eq $shown) { $shown = "PS $($loc.Path)> " }
    "$out$(-join $shown)$e]133;B$a"
  }
  if (Get-Command PSConsoleHostReadLine -ErrorAction Ignore) {
    $global:__ptyro_readline = $function:PSConsoleHostReadLine
    function global:PSConsoleHostReadLine {
      $line = & $global:__ptyro_readline
      $global:__ptyro_lec = $global:LASTEXITCODE
      $global:__ptyro_err = if ($global:Error.Count) { $global:Error[0] } else { $null }
      $c = ("$line" -replace '[\x00-\x1f\x7f]', ' ').Trim()
      if ($c) {
        if ($c.Length -gt 200) { $c = $c.Substring(0, 200) }
        [Console]::Write("$([char]27)]133;C;cmdline=$c$([char]7)")
      }
      $line
    }
  }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_default_shell_is_shell_on_unix_and_powershell_first_on_windows() {
        let env = |k: &str| match k {
            "SHELL" => Some("/bin/zsh".to_owned()),
            "COMSPEC" => Some(r"C:\Windows\system32\cmd.exe".to_owned()),
            _ => None,
        };
        if cfg!(windows) {
            assert_eq!(default_shell_from(&env, &|_| true), "pwsh.exe");
            assert_eq!(default_shell_from(&env, &|p| p == "powershell.exe"), "powershell.exe");
            assert_eq!(default_shell_from(&env, &|_| false), r"C:\Windows\system32\cmd.exe");
            assert_eq!(default_shell_from(&|_| None, &|_| false), "cmd.exe");
        } else {
            assert_eq!(default_shell_from(&env, &|_| true), "/bin/zsh");
            assert_eq!(default_shell_from(&|_| None, &|_| true), "/bin/sh");
        }
    }

    #[test]
    fn each_shell_runs_a_command_line_its_own_way() {
        assert_eq!(command_argv("sh", "make test"), v(&["sh", "-c", "make test"]));
        assert_eq!(command_argv("/bin/zsh", "a | b"), v(&["/bin/zsh", "-c", "a | b"]));
        assert_eq!(command_argv("fish", "x"), v(&["fish", "-c", "x"]));
        assert_eq!(
            command_argv("pwsh.exe", "Get-Date"),
            v(&["pwsh.exe", "-NoProfile", "-Command", "Get-Date"])
        );
        assert_eq!(
            command_argv(r"C:\WINDOWS\system32\WindowsPowerShell\v1.0\powershell.exe", "x")[1],
            "-NoProfile"
        );
        assert_eq!(
            command_argv(r"C:\Windows\system32\cmd.exe", "dir"),
            v(&[r"C:\Windows\system32\cmd.exe", "/d", "/s", "/c", "dir"])
        );
        assert_eq!(command_argv("CMD.EXE", "dir")[1], "/d");
        if !cfg!(windows) {
            assert_eq!(command_shell("echo hi"), v(&["sh", "-c", "echo hi"]));
        }
    }

    #[test]
    fn an_editor_gets_the_path_unparsed_or_quoted_for_its_shell() {
        if !cfg!(windows) {
            let argv = editor_command("vim +3", Path::new("/a b/$x's.toml"));
            assert_eq!(argv, ["sh", "-c", "vim +3 \"$1\"", "sh", "/a b/$x's.toml"]);
        }
        assert_eq!(
            editor_command_in("pwsh.exe", "code --wait", r"C:\it's\c.toml"),
            v(&["pwsh.exe", "-NoProfile", "-Command", r"& code --wait 'C:\it''s\c.toml'"])
        );
        assert_eq!(
            editor_command_in("cmd.exe", "notepad", r"C:\a b\c.toml"),
            v(&["cmd.exe", "/d", "/s", "/c", r#"notepad "C:\a b\c.toml""#])
        );
    }

    #[cfg(unix)] // runs sh
    #[test]
    fn the_unix_editor_line_reaches_the_editor_with_the_path_intact() {
        let path = Path::new("/tmp/a b/$HOME's \"x\".toml");
        let argv = editor_command("printf '%s'", path);
        let out = std::process::Command::new(&argv[0]).args(&argv[1..]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), path.to_string_lossy());
    }

    #[test]
    fn programs_are_known_by_their_name_alone() {
        assert_eq!(ShellKind::of_program("/bin/zsh"), Some(ShellKind::Zsh));
        assert_eq!(ShellKind::of_program("-bash"), Some(ShellKind::Bash));
        assert_eq!(ShellKind::of_program("/opt/homebrew/bin/fish"), Some(ShellKind::Fish));
        assert_eq!(ShellKind::of_program("pwsh"), Some(ShellKind::Pwsh));
        assert_eq!(
            ShellKind::of_program(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            Some(ShellKind::Pwsh)
        );
        assert_eq!(ShellKind::of_program("PowerShell.EXE"), Some(ShellKind::Pwsh));
        assert_eq!(ShellKind::of_program("pwsh-preview"), Some(ShellKind::Pwsh));
        assert_eq!(ShellKind::of_program("/bin/sh"), None);
        assert_eq!(ShellKind::of_program("bashful"), None);
    }

    #[test]
    fn only_a_bare_shell_is_an_interactive_one() {
        assert_eq!(interactive(&v(&["zsh", "-l"])), Some(ShellKind::Zsh));
        assert_eq!(interactive(&v(&["bash"])), Some(ShellKind::Bash));
        assert_eq!(interactive(&v(&["pwsh", "-NoLogo", "-Login"])), Some(ShellKind::Pwsh));
        assert_eq!(interactive(&v(&["bash", "-c", "make"])), None);
        assert_eq!(interactive(&v(&["pwsh", "-File", "x.ps1"])), None);
        assert_eq!(interactive(&v(&["zsh", "script.zsh"])), None);
        assert_eq!(interactive(&v(&["vim"])), None);
        assert_eq!(interactive(&[]), None);
    }

    #[test]
    fn every_snippet_reports_marks_and_the_directory_only_inside_ptyro() {
        for k in ShellKind::ALL {
            let s = k.snippet();
            assert!(s.contains("133;A") && s.contains("133;C;cmdline="), "{k:?}");
            // fish's prompt isn't wrapped, so it has no B: the mark ends at C, D or a newline.
            assert!(k == ShellKind::Fish || s.contains("133;B"), "{k:?}");
            assert!(s.contains("133;D"), "{k:?}");
            assert!(s.contains("]7;file://"), "{k:?}");
            assert!(s.contains("PTYRO") && s.contains("__ptyro_integrated"), "{k:?}");
            assert!(
                s.contains(&format!("ptyro shell-init {}", k.name())),
                "{k:?}: says how to add it"
            );
        }
        assert!(PWSH.contains("]9;9;"), "Windows Terminal's directory report on Windows");
    }

    #[cfg(unix)] // shells start in the daemon's sessions (Unix)
    #[test]
    fn bash_reads_the_login_files_or_bashrc_then_the_snippet() {
        let dir = Path::new("/d");
        let i = integrate(&v(&["/bin/bash", "-l"]), dir, None).unwrap();
        assert_eq!(i.argv, v(&["/bin/bash", "--init-file", "/d/bash-init", "-i"]));
        assert_eq!(i.env, vec![("PTYRO_BASH_LOGIN".to_owned(), "1".to_owned())]);
        let init = &i.files.iter().find(|(p, _)| p.ends_with("bash-init")).unwrap().1;
        assert!(init.contains(".bash_profile") && init.contains(".bashrc"));
        assert!(init.trim_end().ends_with(". '/d/ptyro.bash'"), "{init}");
        assert!(integrate(&v(&["bash"]), dir, None).unwrap().env.is_empty(), "not a login shell");
    }

    #[cfg(unix)] // shells start in the daemon's sessions (Unix)
    #[test]
    fn zsh_starts_from_ptyros_zdotdir_and_hands_the_users_back() {
        let i = integrate(&v(&["/bin/zsh", "-l"]), Path::new("/d"), None).unwrap();
        assert_eq!(i.argv, v(&["/bin/zsh", "-l"]), "the command itself is unchanged");
        assert!(i.env.contains(&("ZDOTDIR".to_owned(), "/d/zsh".to_owned())));
        assert!(i.env.contains(&("PTYRO_ZDOTDIR_UNSET".to_owned(), "1".to_owned())));
        let names: Vec<_> = i.files.iter().map(|(p, _)| p.display().to_string()).collect();
        for f in [
            "/d/zsh/.zshenv",
            "/d/zsh/.zprofile",
            "/d/zsh/.zshrc",
            "/d/zsh/.zlogin",
            "/d/ptyro.zsh",
        ] {
            assert!(names.iter().any(|n| n == f), "{f} in {names:?}");
        }
        let with = integrate(&v(&["zsh"]), Path::new("/d"), Some("/home/me/.config/zsh")).unwrap();
        assert!(
            with.env.contains(&("PTYRO_ZDOTDIR".to_owned(), "/home/me/.config/zsh".to_owned()))
        );
    }

    #[cfg(unix)] // shells start in the daemon's sessions (Unix)
    #[test]
    fn fish_and_pwsh_load_the_snippet_after_their_own_config() {
        let f = integrate(&v(&["fish", "-l"]), Path::new("/d"), None).unwrap();
        assert_eq!(f.argv, v(&["fish", "-l", "--init-command", "source '/d/ptyro.fish'"]));
        let p = integrate(&v(&["pwsh", "-l"]), Path::new("/it's"), None).unwrap();
        let login: &[&str] = if cfg!(windows) { &[] } else { &["-Login"] };
        let mut want = vec!["pwsh"];
        want.extend(login);
        want.extend(["-NoLogo", "-NoExit", "-Command", ". '/it''s/ptyro.ps1'"]);
        assert_eq!(p.argv, v(&want));
        let np = integrate(&v(&["pwsh", "-NoProfile"]), Path::new("/d"), None).unwrap();
        assert_eq!(np.argv[..3], v(&["pwsh", "-NoProfile", "-NoLogo"]), "the user's switches stay");
        assert!(integrate(&v(&["sh"]), Path::new("/d"), None).is_none());
        assert!(integrate(&v(&["bash", "-c", "x"]), Path::new("/d"), None).is_none());
    }
}
