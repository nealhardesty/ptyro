#!/bin/sh
# Prove scripts/install.sh installs what `make dist` built (`make dist-check`): the tarball in
# dist/ goes through the installer into a temporary prefix, with the release's SHA256SUMS made
# from the .sha256 files, and the installed binary has to run and report the right version. A bad
# checksum must refuse.
set -eu
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TARGET=${TARGET:-$(rustc -vV | sed -n 's/^host: //p')}
NAME=ptyro-$VERSION-$TARGET
[ -f "dist/$NAME.tar.gz" ] || { echo "dist-check: dist/$NAME.tar.gz is missing: run \`make dist\` first" >&2; exit 1; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/release"
cp "dist/$NAME.tar.gz" "$work/release/"
cat "dist/$NAME.tar.gz.sha256" > "$work/release/SHA256SUMS"
run() {
  env -u PTYRO_SOCKET -u PTYRO_SESSION_ID -u PTYRO_CONFIG HOME="$work/home" PTYRO_VERIFY=0 \
    PTYRO_PREFIX="$work/prefix" PTYRO_VERSION="$VERSION" PTYRO_RELEASE_URL="file://$work/release" \
    sh scripts/install.sh
}
mkdir -p "$work/home"
run
got=$("$work/prefix/bin/ptyro" --version)
[ "$got" = "ptyro $VERSION" ] || { echo "dist-check: installed binary says '$got', expected 'ptyro $VERSION'" >&2; exit 1; }
[ -f "$work/prefix/share/ptyro/completions/_ptyro" ] || { echo "dist-check: completions weren't installed" >&2; exit 1; }
# A tampered download is refused, and the good install is left alone.
echo tampered >> "$work/release/$NAME.tar.gz"
if run >"$work/out" 2>&1; then echo "dist-check: a tampered tarball was installed" >&2; exit 1; fi
grep -q "checksum" "$work/out" || { echo "dist-check: the refusal doesn't say why" >&2; cat "$work/out" >&2; exit 1; }
"$work/prefix/bin/ptyro" --version >/dev/null
# --uninstall, piped as from curl, hands over to `ptyro uninstall`. Everything ptyro could find
# (its directories, a runtime dir, a daemon) is under $work: no XDG variable points elsewhere.
uninstall() {
  env -u PTYRO_SOCKET -u PTYRO_SESSION_ID -u PTYRO_CONFIG -u PTYRO_HOME -u XDG_RUNTIME_DIR \
    -u XDG_CONFIG_HOME -u XDG_DATA_HOME -u XDG_STATE_HOME -u XDG_CACHE_HOME \
    HOME="$work/home" TMPDIR="$work/tmp" PTYRO_PREFIX="$work/prefix" \
    sh -s -- --uninstall "$@" <scripts/install.sh
}
mkdir -p "$work/tmp"
uninstall --dry-run >"$work/out" 2>&1 || { echo "dist-check: --uninstall --dry-run failed" >&2; cat "$work/out" >&2; exit 1; }
[ -x "$work/prefix/bin/ptyro" ] || { echo "dist-check: --uninstall --dry-run removed the binary" >&2; exit 1; }
uninstall --yes >"$work/out" 2>&1 || { echo "dist-check: --uninstall --yes failed" >&2; cat "$work/out" >&2; exit 1; }
[ ! -e "$work/prefix/bin/ptyro" ] || { echo "dist-check: --uninstall left the binary" >&2; cat "$work/out" >&2; exit 1; }
[ ! -e "$work/prefix/share/ptyro/completions" ] || { echo "dist-check: --uninstall left the completions" >&2; exit 1; }
# A binary that doesn't run can't uninstall itself: the script removes what it installed.
mkdir -p "$work/prefix/bin" && printf 'broken\n' >"$work/prefix/bin/ptyro" && chmod 755 "$work/prefix/bin/ptyro"
uninstall --dry-run >"$work/out" 2>&1 && [ -e "$work/prefix/bin/ptyro" ] ||
  { echo "dist-check: --uninstall --dry-run removed a broken binary" >&2; cat "$work/out" >&2; exit 1; }
uninstall >"$work/out" 2>&1 && [ ! -e "$work/prefix/bin/ptyro" ] ||
  { echo "dist-check: --uninstall didn't remove a broken binary" >&2; cat "$work/out" >&2; exit 1; }
echo "dist-check ok: $NAME installs, runs, a tampered tarball is refused, and --uninstall removes it"
