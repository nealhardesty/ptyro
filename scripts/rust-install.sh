#!/bin/sh
# Install Rust for building ptyro: rustup (if missing), then the toolchain pinned in
# rust-toolchain.toml with its components. Linux, macOS and native Windows (from Git Bash, MSYS2
# or Cygwin). Run via `make rust-install`, or `sh scripts/rust-install.sh` where there is no make.
# rustup-init is downloaded from static.rust-lang.org and checked against its published SHA-256
# before it runs (no `curl | sh`). Never uses sudo: a missing C linker is reported, not installed.
set -eu
cd "$(dirname "$0")/.."

say() { printf 'rust-install: %s\n' "$*"; }
die() { say "$*" >&2; exit 1; }
arch= exe= cargo_bin=

# The C toolchain rustc links with.
case $(uname -s) in
  Darwin)
    xcode-select -p >/dev/null 2>&1 ||
      die "the Xcode command line tools are missing: run 'xcode-select --install', then retry"
    os=apple-darwin ;;
  Linux)
    command -v cc >/dev/null 2>&1 ||
      die "no C compiler (cc) found: install one first (Debian/Ubuntu: sudo apt install build-essential; Fedora: sudo dnf install gcc; Arch: sudo pacman -S base-devel; Alpine: sudo apk add build-base)"
    if ldd --version 2>&1 | grep -qi musl; then os=unknown-linux-musl; else os=unknown-linux-gnu; fi ;;
  MINGW* | MSYS* | CYGWIN*)
    # rustc links with MSVC: the Visual Studio Build Tools' C++ workload brings it and the Windows
    # SDK. uname -m and PROCESSOR_ARCHITECTURE name the shell's own architecture, which is AMD64
    # when an x64 Git Bash or MSYS2 runs emulated on ARM64; PROCESSOR_IDENTIFIER names the CPU.
    cpu="${PROCESSOR_ARCHITECTURE:-} ${PROCESSOR_ARCHITEW6432:-} ${PROCESSOR_IDENTIFIER:-}"
    case $cpu in
      *ARM*) arch=aarch64 vc=ARM64 ;;
      *AMD64*) arch=x86_64 vc=x86.x64 ;;
      *) die "unsupported CPU ($cpu): only x86_64 and aarch64 for now" ;;
    esac
    pf=$(printenv 'ProgramFiles(x86)' || true)
    vswhere="$(cygpath -u "${pf:-C:\Program Files (x86)}")/Microsoft Visual Studio/Installer/vswhere.exe"
    [ -x "$vswhere" ] &&
      [ -n "$("$vswhere" -nologo -latest -products '*' -requires "Microsoft.VisualStudio.Component.VC.Tools.$vc" -property installationPath)" ] ||
      die "the MSVC C++ build tools are missing: run 'winget install Microsoft.VisualStudio.BuildTools --override \"--passive --wait --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.VC.Tools.$vc --includeRecommended\"' (if Build Tools is already installed, add \"Desktop development with C++\" in the Visual Studio Installer instead), then retry in a new terminal"
    os=pc-windows-msvc exe=.exe
    # rustup installs to %USERPROFILE%, which MSYS2 (unlike Git Bash) doesn't make $HOME.
    cargo_bin="$(cygpath -u "$USERPROFILE")/.cargo/bin" ;;
  *) die "unsupported OS $(uname -s): only Linux, macOS and Windows for now" ;;
esac
[ -n "$cargo_bin" ] || cargo_bin=$HOME/.cargo/bin

# `make` already puts the cargo bin directory on PATH; the caller's own shell may not have it.
on_path=
case :${PTYRO_CALLER_PATH:-$PATH}: in
  *":$cargo_bin:"*) on_path=yes ;;
esac
export PATH="$cargo_bin:$PATH"

# Only the official install in ~/.cargo/bin counts: a package-manager rustup (Homebrew, apt,
# Scoop) does not put cargo and rustc on PATH, and Makefile expects them in ~/.cargo/bin.
if [ -x "$cargo_bin/rustup$exe" ]; then
  say "rustup is already installed ($("$cargo_bin/rustup$exe" --version 2>/dev/null | head -1))"
else
  [ -n "$arch" ] || case $(uname -m) in
    x86_64 | amd64) arch=x86_64 ;;
    arm64 | aarch64) arch=aarch64 ;;
    *) die "unsupported CPU $(uname -m): only x86_64 and aarch64 for now" ;;
  esac
  triple=$arch-$os
  url=https://static.rust-lang.org/rustup/dist/$triple/rustup-init$exe
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  say "downloading rustup-init for $triple"
  if command -v curl >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -fsSL -o "$tmp/rustup-init$exe" "$url"
    curl --proto '=https' --tlsv1.2 -fsSL -o "$tmp/rustup-init.sha256" "$url.sha256"
  elif command -v wget >/dev/null 2>&1; then
    wget --https-only -q -O "$tmp/rustup-init$exe" "$url"
    wget --https-only -q -O "$tmp/rustup-init.sha256" "$url.sha256"
  else
    die "neither curl nor wget is installed"
  fi
  want=$(cut -d' ' -f1 "$tmp/rustup-init.sha256")
  if command -v sha256sum >/dev/null 2>&1; then
    got=$(sha256sum "$tmp/rustup-init$exe" | cut -d' ' -f1)
  else
    got=$(shasum -a 256 "$tmp/rustup-init$exe" | cut -d' ' -f1)
  fi
  [ -n "$want" ] && [ "$want" = "$got" ] || die "rustup-init checksum mismatch (want $want, got $got)"
  chmod +x "$tmp/rustup-init$exe"
  # No default toolchain: the pinned one below is all ptyro needs.
  "$tmp/rustup-init$exe" -y --default-toolchain none
fi

say "installing the toolchain pinned in rust-toolchain.toml"
# rustup 1.28+ installs the toolchain file's pin; older ones do it on first use (`rustup show`).
rustup toolchain install 2>/dev/null || rustup show active-toolchain
say "ready: $(rustc --version), $(cargo --version)"
if [ -z "$on_path" ]; then
  # On Windows rustup-init puts cargo on the user PATH; there is no ~/.cargo/env to source. MSYS2
  # leaves the Windows PATH out of its shells by default (MSYS2_PATH_TYPE=minimal).
  if [ -n "$exe" ]; then
    say "open a new terminal to put cargo on your PATH (in MSYS2, whose shells leave out the Windows PATH, also add 'export PATH=\"$cargo_bin:\$PATH\"' to ~/.bashrc)"
  else
    say "open a new shell, or run '. \"\$HOME/.cargo/env\"', to put cargo on your PATH"
  fi
fi
