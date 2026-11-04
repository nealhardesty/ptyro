#!/usr/bin/env sh
# `make check-windows` (ROADMAP WIN0.4, then WIN3): cross-check ptyro for Windows from Linux or
# macOS with cargo-xwin, for x64 and ARM64. No Windows machine or Visual Studio needed.
#
# Today it checks what WIN0.4 proved builds: the C libraries ptyro bundles (ring, SQLite, zstd),
# which need the right compiler setup. ptyro-core still stops a Windows build on purpose
# (its compile_error!), so the ptyro crates join this list as WIN3 makes each build (CRATES).
#
#   make check-windows                     x64 and ARM64
#   make check-windows TARGETS=x86_64      one of them
#
# cargo-xwin downloads Microsoft's CRT and Windows SDK, under Microsoft's licence. It asks
# nothing itself, so this script asks you to accept it once: XWIN_ACCEPT_LICENSE=1.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
targets=${TARGETS:-"x86_64 aarch64"}
# What WIN0.4 found builds; WIN3 adds the ptyro crates here as they compile (SPEC §20.11).
crates=${CRATES:-"ring libsqlite3-sys zstd-sys"}

missing=
need() { command -v "$1" >/dev/null 2>&1 || missing="$missing $1"; }
need clang
need lld-link
need llvm-lib
need cargo-xwin
if [ -n "$missing" ]; then
  echo "check-windows: missing:$missing" >&2
  case "$(uname -s)" in
    Darwin) echo "  brew install llvm lld && cargo install --locked cargo-xwin  (and put \$(brew --prefix llvm)/bin on PATH)" >&2 ;;
    *) echo "  sudo apt install clang lld llvm && cargo install --locked cargo-xwin" >&2 ;;
  esac
  exit 1
fi

for t in $targets; do
  if ! rustup target list --installed 2>/dev/null | grep -qx "$t-pc-windows-msvc"; then
    echo "check-windows: adding the Rust target $t-pc-windows-msvc"
    rustup target add "$t-pc-windows-msvc"
  fi
done

if [ "${XWIN_ACCEPT_LICENSE:-}" != 1 ] && [ ! -d "${XWIN_CACHE_DIR:-$HOME/.cache/cargo-xwin}/xwin/crt" ]; then
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
  echo "check-windows: $t-pc-windows-msvc:$pkgs"
  # shellcheck disable=SC2086
  cargo xwin check --target "$t-pc-windows-msvc" $pkgs
done
echo "check-windows: ok ($targets)"
