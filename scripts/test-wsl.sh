#!/usr/bin/env sh
# ROADMAP WIN2: CI's main job (`make check`) inside a WSL2 distro. CI's `wsl` job runs it on a
# hosted Windows runner (WIN0.5 f); run it by hand on your own WSL before a release too.
#
#   inside WSL:              make test-wsl                (this checkout)
#   on Windows (Git Bash):   make test-wsl                (the checkout inside the distro)
#                            make test-wsl DISTRO=Ubuntu WSL_DIR=~/src/ptyro
set -eu

in_wsl() {
  [ -n "${WSL_DISTRO_NAME:-}" ] || grep -qi microsoft /proc/sys/kernel/osrelease 2>/dev/null
}

if in_wsl; then
  root=$(cd "$(dirname "$0")/.." && pwd)
  case "$root" in
    /mnt/[a-z]/*) echo "test-wsl: note: $root is on a Windows drive; the build and tests run many times slower there" >&2 ;;
  esac
  # Tests that run inside a ptyro session must never reach the real daemon.
  unset PTYRO_SOCKET PTYRO_SESSION_ID PTYRO_CONFIG
  # A shell rc that sets PATH outright can drop rustup's directory.
  PATH="$HOME/.cargo/bin:$PATH"
  export PATH
  echo "test-wsl: $(uname -r), ${WSL_DISTRO_NAME:-distro unknown}, in $root"
  cd "$root"
  exec make check
fi

if command -v wsl.exe >/dev/null 2>&1; then
  # Left for the distro's bash to expand: here `~` would be the Windows home.
  dir=${WSL_DIR:-"~/dev/ptyro"}
  echo "test-wsl: make check in ${DISTRO:-the default distro}, $dir"
  # A login shell, so the distro's own PATH (and rustup's) applies. MSYS2 and Git Bash would
  # rewrite a path-like argument; MSYS2_ARG_CONV_EXCL stops that.
  run="cd $dir && make test-wsl"
  if [ -n "${DISTRO:-}" ]; then
    MSYS2_ARG_CONV_EXCL='*' exec wsl.exe -d "$DISTRO" --cd "~" -- bash -lc "$run"
  fi
  MSYS2_ARG_CONV_EXCL='*' exec wsl.exe --cd "~" -- bash -lc "$run"
fi

echo "test-wsl: run it inside WSL2, or on Windows where wsl.exe is (Git Bash, MSYS2)" >&2
exit 1
