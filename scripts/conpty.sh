#!/usr/bin/env sh
# `make conpty` (ROADMAP WIN5, WIN0.5 g): Microsoft's packaged ConPTY, which ptyro ships beside
# ptyro.exe: conpty.dll and the OpenConsole.exe it starts. It passes a program's bytes through
# as they are, where Windows' own console host repaints them (SPEC §20.13).
#
# Fetches one pinned NuGet package (Microsoft.Windows.Console.ConPTY, MIT), checks its SHA-256,
# and leaves each architecture's two files in target/conpty/<x86_64|aarch64>/. Runs anywhere
# with curl and unzip, or Windows' tar.exe (Linux, macOS, WSL, Git Bash). The pin was taken from a download whose
# four files Windows verified as signed by Microsoft (Get-AuthenticodeSignature, 2026-11-04).
#
#   make conpty                     both architectures
#   make conpty INTO=<dir> [ARCH=x86_64|aarch64]   also copy one architecture's files into <dir>
#                                   (beside a ptyro.exe; ARCH defaults to this machine's)
set -eu

version=1.25.260930003
sha256=02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee

root=$(cd "$(dirname "$0")/.." && pwd)
out="$root/target/conpty"
pkg="$out/microsoft.windows.console.conpty.$version.nupkg"
url="https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$version/microsoft.windows.console.conpty.$version.nupkg"

command -v curl >/dev/null 2>&1 || { echo "conpty: needs curl" >&2; exit 1; }
# One file out of the zip into a folder: unzip, or Windows' own bsdtar (Git Bash may lack unzip).
winTar=/c/Windows/System32/tar.exe
extract() {
  if command -v unzip >/dev/null 2>&1; then
    unzip -o -q -j "$1" "$2" -d "$3"
  elif [ -x "$winTar" ]; then
    tmp=$(mktemp -d)
    "$winTar" -xf "$(cygpath -w "$1" 2>/dev/null || echo "$1")" -C "$(cygpath -w "$tmp" 2>/dev/null || echo "$tmp")" "$2"
    cp "$tmp/$2" "$3/"
    rm -rf "$tmp"
  else
    echo "conpty: needs unzip (or Windows' tar.exe)" >&2
    exit 1
  fi
}
sum() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

mkdir -p "$out"
if [ ! -f "$pkg" ] || [ "$(sum "$pkg")" != "$sha256" ]; then
  echo "conpty: fetching Microsoft.Windows.Console.ConPTY $version"
  curl -fsSL --retry 3 -o "$pkg.part" "$url"
  got=$(sum "$pkg.part")
  if [ "$got" != "$sha256" ]; then
    rm -f "$pkg.part"
    echo "conpty: the package's SHA-256 is $got, not the pinned $sha256: refusing it" >&2
    exit 1
  fi
  mv "$pkg.part" "$pkg"
fi

# NuGet's runtime names, by Rust's.
for arch in x86_64 aarch64; do
  case $arch in x86_64) rid=x64 ;; aarch64) rid=arm64 ;; esac
  dir="$out/$arch"
  mkdir -p "$dir"
  extract "$pkg" "runtimes/win-$rid/native/conpty.dll" "$dir"
  extract "$pkg" "build/native/runtimes/$rid/OpenConsole.exe" "$dir"
done
echo "conpty: $version in target/conpty/{x86_64,aarch64}/ (conpty.dll, OpenConsole.exe)"

if [ -n "${INTO:-}" ]; then
  # This machine's architecture unless told (Git Bash on ARM64 Windows says aarch64 or arm64).
  case "${ARCH:-$(uname -m)}" in aarch64|arm64|ARM64) arch=aarch64 ;; *) arch=x86_64 ;; esac
  mkdir -p "$INTO"
  cp "$out/$arch/conpty.dll" "$out/$arch/OpenConsole.exe" "$INTO/"
  echo "conpty: copied the $arch files into $INTO"
fi
