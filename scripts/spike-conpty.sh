#!/bin/sh
# WIN0.2 and WIN0.3 (`make spike-conpty`): play what ptyro needs through ConPTY, Windows' own
# (conhost) and Microsoft's newer one (conpty.dll + OpenConsole.exe from the NuGet package
# Microsoft.Windows.Console.ConPTY), check whether a second process can take a pseudoconsole
# over, and write the findings to spikes/conpty/REPORT.md (SPEC §20.13 has what they decided).
#
# Runs from WSL (it cross-builds with cargo-xwin, as `make check-windows` does, and runs the
# spike on Windows through interop) or on Windows itself in Git Bash (a native build).
#
#   make spike-conpty                    build, run, report
#   make spike-conpty ARGS=report        report again from the last run
#   CONPTY_VERSION=1.26.…                another NuGet version (default: the one below)
set -eu
cd "$(dirname "$0")/.."
root=$(pwd)
version=${CONPTY_VERSION:-1.25.260930003}
report="$root/spikes/conpty/REPORT.md"

case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) where=windows ;;
  *) if [ -n "${WSL_DISTRO_NAME:-}" ] || grep -qi microsoft /proc/sys/kernel/osrelease 2>/dev/null; then where=wsl; else where=other; fi ;;
esac
if [ "$where" = other ]; then
  echo "spike-conpty: it runs on Windows: from WSL, or in Git Bash on Windows" >&2
  exit 1
fi

# Windows PowerShell: on PATH, else where Windows keeps it (WSL may leave Windows' PATH out).
pwsh=powershell.exe
if ! command -v "$pwsh" >/dev/null 2>&1; then
  if [ "$where" = wsl ]; then pwsh=$(wslpath 'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'); fi
  [ -x "$pwsh" ] || { echo "spike-conpty: can't find powershell.exe" >&2; exit 1; }
fi
# Its output without CRs; its failure is ours (a pipe would hide it).
ps() {
  r=$(cd / && "$pwsh" -NoProfile -ExecutionPolicy Bypass -Command "$1") || return 1
  printf '%s\n' "$r" | tr -d '\r'
}
# The spike's folder on Windows, in both spellings.
win_dir=$(ps '[IO.Path]::Combine($env:LOCALAPPDATA, "Temp", "ptyro-spike")') || win_dir=
[ -n "$win_dir" ] || { echo "spike-conpty: PowerShell didn't say where the temp folder is" >&2; exit 1; }
if [ "$where" = wsl ]; then dir=$(wslpath -u "$win_dir"); else dir=$(cygpath -u "$win_dir"); fi
out="$dir/out"

if [ "${1:-}" != report ]; then
  # A debug build: Smart App Control refused this spike's release build on the owner's machine
  # and ran its debug build (SPEC §20.12).
  echo "spike-conpty: building"
  if [ "$where" = wsl ]; then
    PATH="$root/scripts/xwin:$PATH" XWIN_ACCEPT_LICENSE=1 CARGO_TARGET_DIR="$root/spikes/conpty/target/windows" \
      cargo xwin build --manifest-path spikes/conpty/Cargo.toml --target x86_64-pc-windows-msvc
    exe="$root/spikes/conpty/target/windows/x86_64-pc-windows-msvc/debug/spike-conpty.exe"
  else
    cargo build --manifest-path spikes/conpty/Cargo.toml
    exe="$root/spikes/conpty/target/debug/spike-conpty.exe"
  fi
  mkdir -p "$dir/fixtures"
  cp "$exe" "$dir/"
  cp crates/ptyro-term/tests/fixtures/*.ptylog "$dir/fixtures/"

  # The newer ConPTY: Microsoft's package, its files checked to be Microsoft's before use.
  echo "spike-conpty: Microsoft.Windows.Console.ConPTY $version"
  dll=$(ps "\$ErrorActionPreference = 'Stop'; \$ProgressPreference = 'SilentlyContinue'
    \$d = '$win_dir'; \$v = '$version'; \$pkg = Join-Path \$d \"nuget-\$v\"
    if (-not (Test-Path \"\$pkg\\runtimes\")) {
      \$zip = Join-Path \$d \"conpty-\$v.zip\"
      Invoke-WebRequest -UseBasicParsing \"https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/\$v/microsoft.windows.console.conpty.\$v.nupkg\" -OutFile \$zip
      Expand-Archive -Force \$zip \$pkg
    }
    \$c = Join-Path \$d 'conpty'; New-Item -ItemType Directory -Force \$c | Out-Null
    Copy-Item \"\$pkg\\runtimes\\win-x64\\native\\conpty.dll\", \"\$pkg\\build\\native\\runtimes\\x64\\OpenConsole.exe\" \$c -Force
    foreach (\$f in 'conpty.dll', 'OpenConsole.exe') {
      \$s = Get-AuthenticodeSignature (Join-Path \$c \$f)
      if (\$s.Status -ne 'Valid' -or \$s.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') { throw \"\$f isn't signed by Microsoft: \$(\$s.Status)\" }
    }
    Join-Path \$c 'conpty.dll'" 2>/dev/null) || { echo "spike-conpty: couldn't get the NuGet ConPTY: running Windows' own only" >&2; dll=-; }

  rm -rf "$out"
  mkdir -p "$out"
  ps "\$cv = Get-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion'
    'Windows build {0}.{1} ({2}; build 22000 and later is Windows 11)' -f \$cv.CurrentBuild, \$cv.UBR, \$cv.DisplayVersion
    'conhost: ' + (Get-Item C:\\Windows\\System32\\conhost.exe).VersionInfo.FileVersion
    if ('$dll' -ne '-') { 'conpty.dll (NuGet Microsoft.Windows.Console.ConPTY $version): ' + (Get-Item '$dll').VersionInfo.FileVersion }
    'arch: ' + \$env:PROCESSOR_ARCHITECTURE" > "$out/environment.txt"
  echo "spike-conpty: running (about 3 minutes)"
  if ! ps "try { & '$win_dir\\spike-conpty.exe' run '$win_dir\\out' '$dll'; exit \$LASTEXITCODE } catch { Write-Output \"\$_\"; exit 1 }" > "$dir/run.log" 2>&1; then
    cat "$dir/run.log" >&2
    if grep -q "Application Control" "$dir/run.log"; then
      echo "spike-conpty: Windows' Smart App Control refused the unsigned spike (it judges each build; SPEC §20.12). Change a line in spikes/conpty and run it again, or run it where Smart App Control is off." >&2
    fi
    echo "spike-conpty: the run failed" >&2
    exit 1
  fi
fi

echo "spike-conpty: report"
cargo run -q --release --manifest-path spikes/conpty/Cargo.toml -- report "$out" > "$report"
echo "spike-conpty: wrote $report"
