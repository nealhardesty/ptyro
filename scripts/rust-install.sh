#!/bin/sh
# Install Rust for building ptyro: rustup (if missing), then the toolchain pinned in
# rust-toolchain.toml with its components. Linux and macOS. Run via `make rust-install`.
# rustup-init is downloaded from static.rust-lang.org and checked against its published SHA-256
# before it runs (no `curl | sh`). Never uses sudo: a missing C linker is reported, not installed.
set -eu
cd "$(dirname "$0")/.."
# `make` already puts ~/.cargo/bin on PATH; the caller's own shell may not have it.
on_path=
case :${PTYRO_CALLER_PATH:-$PATH}: in
  *":$HOME/.cargo/bin:"*) on_path=yes ;;
esac
export PATH="$HOME/.cargo/bin:$PATH"

say() { printf 'rust-install: %s\n' "$*"; }
die() { say "$*" >&2; exit 1; }

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
  *) die "unsupported OS $(uname -s): only Linux and macOS for now" ;;
esac

# Only the official install in ~/.cargo/bin counts: a package-manager rustup (Homebrew, apt) does
# not put cargo and rustc on PATH, and Makefile expects them in ~/.cargo/bin.
if [ -x "$HOME/.cargo/bin/rustup" ]; then
  say "rustup is already installed ($("$HOME/.cargo/bin/rustup" --version 2>/dev/null | head -1))"
else
  case $(uname -m) in
    x86_64 | amd64) arch=x86_64 ;;
    arm64 | aarch64) arch=aarch64 ;;
    *) die "unsupported CPU $(uname -m): only x86_64 and aarch64 for now" ;;
  esac
  triple=$arch-$os
  url=https://static.rust-lang.org/rustup/dist/$triple/rustup-init
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  say "downloading rustup-init for $triple"
  if command -v curl >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -fsSL -o "$tmp/rustup-init" "$url"
    curl --proto '=https' --tlsv1.2 -fsSL -o "$tmp/rustup-init.sha256" "$url.sha256"
  elif command -v wget >/dev/null 2>&1; then
    wget --https-only -q -O "$tmp/rustup-init" "$url"
    wget --https-only -q -O "$tmp/rustup-init.sha256" "$url.sha256"
  else
    die "neither curl nor wget is installed"
  fi
  want=$(cut -d' ' -f1 "$tmp/rustup-init.sha256")
  if command -v sha256sum >/dev/null 2>&1; then
    got=$(sha256sum "$tmp/rustup-init" | cut -d' ' -f1)
  else
    got=$(shasum -a 256 "$tmp/rustup-init" | cut -d' ' -f1)
  fi
  [ -n "$want" ] && [ "$want" = "$got" ] || die "rustup-init checksum mismatch (want $want, got $got)"
  chmod +x "$tmp/rustup-init"
  # No default toolchain: the pinned one below is all ptyro needs.
  "$tmp/rustup-init" -y --default-toolchain none
fi

say "installing the toolchain pinned in rust-toolchain.toml"
# rustup 1.28+ installs the toolchain file's pin; older ones do it on first use (`rustup show`).
rustup toolchain install 2>/dev/null || rustup show active-toolchain
say "ready: $(rustc --version), $(cargo --version)"
[ -n "$on_path" ] || say "open a new shell, or run '. \"\$HOME/.cargo/env\"', to put cargo on your PATH"
