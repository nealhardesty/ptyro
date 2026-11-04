#!/usr/bin/env sh
# `make check-windows` (ROADMAP WIN0.4, then WIN3): cross-check ptyro for Windows from Linux or
# macOS with cargo-xwin, for x64 and ARM64. No Windows machine or Visual Studio needed.
#
# It checks the Windows client (WIN3): every crate it is made of, with its tests, and the ptyro
# binary (whose own tests need a daemon, so they are Unix's). The daemon side (ptyro-daemon,
# ptyro-pty, ptyro-testkit) isn't Windows' yet (track D).
#
#   make check-windows                     x64 and ARM64
#   make check-windows TARGETS=x86_64      one of them
#   OPTIONAL=1 (what `make test` sets)     say so and stop, instead of failing, when the tools or
#                                          the licence are missing
#
# cargo-xwin downloads Microsoft's CRT and Windows SDK, under Microsoft's licence. It asks
# nothing itself, so this script asks you to accept it once: XWIN_ACCEPT_LICENSE=1.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
targets=${TARGETS:-"x86_64 aarch64"}
# The Windows client's crates (SPEC §20.11), each with its tests.
crates=${CRATES:-"ptyro-core ptyro-protocol ptyro-term ptyro-store ptyro-agents ptyro-git ptyro-meaning ptyro-run ptyro-drive ptyro-client ptyro-tui"}
optional=${OPTIONAL:-}
skip() { echo "check-windows: skipped: $1 (make check-windows says how)"; exit 0; }

missing=
need() { command -v "$1" >/dev/null 2>&1 || missing="$missing $1"; }
need clang
need lld-link
need llvm-lib
need cargo-xwin
if [ -n "$missing" ]; then
  [ -n "$optional" ] && skip "no$missing"
  echo "check-windows: missing:$missing" >&2
  case "$(uname -s)" in
    Darwin) echo "  brew install llvm lld && cargo install --locked cargo-xwin  (and put \$(brew --prefix llvm)/bin on PATH)" >&2 ;;
    *) echo "  sudo apt install clang lld llvm && cargo install --locked cargo-xwin" >&2 ;;
  esac
  exit 1
fi

for t in $targets; do
  if ! rustup target list --installed 2>/dev/null | grep -qx "$t-pc-windows-msvc"; then
    [ -n "$optional" ] && skip "the Rust target $t-pc-windows-msvc isn't installed"
    echo "check-windows: adding the Rust target $t-pc-windows-msvc"
    rustup target add "$t-pc-windows-msvc"
  fi
done

if [ "${XWIN_ACCEPT_LICENSE:-}" != 1 ] && [ ! -d "${XWIN_CACHE_DIR:-$HOME/.cache/cargo-xwin}/xwin/crt" ]; then
  [ -n "$optional" ] && skip "Microsoft's CRT and SDK aren't downloaded (their licence: XWIN_ACCEPT_LICENSE=1)"
  echo "check-windows: cargo-xwin downloads Microsoft's CRT and Windows SDK (about 1 GB), under" >&2
  echo "  Microsoft's licence (https://go.microsoft.com/fwlink/?LinkId=2086102). To accept it:" >&2
  echo "  make check-windows XWIN_ACCEPT_LICENSE=1" >&2
  exit 1
fi
export XWIN_ACCEPT_LICENSE=1

# ring builds ARM64 with the GNU-style clang driver; scripts/xwin/clang gives it the include
# flags in that form (SPEC §20.11). Nothing else comes through it.
PATH="$root/scripts/xwin:$PATH"
export PATH
# A target dir of its own: the host build's isn't touched.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target/windows}"

pkgs=
for c in $crates; do pkgs="$pkgs -p $c"; done
cd "$root"
for t in $targets; do
  echo "check-windows: $t-pc-windows-msvc: the client crates with their tests, and ptyro.exe"
  # shellcheck disable=SC2086
  cargo xwin check --target "$t-pc-windows-msvc" --all-targets $pkgs
  cargo xwin check --target "$t-pc-windows-msvc" -p ptyro --bins
done
echo "check-windows: ok ($targets)"
