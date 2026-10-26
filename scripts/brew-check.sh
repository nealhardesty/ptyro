#!/bin/sh
# Prove the Homebrew formula (packaging/homebrew/ptyro.rb.in) installs what `make dist` built
# (`make brew-check`): it is rendered for this platform with a file:// URL to dist/'s tarball and
# its real checksum, installed from a throwaway local tap, and has to pass `brew test` and
# `brew audit`, report the crate version, install the completions and print the caveats. A wrong
# checksum must make the install fail. Everything is undone on exit, success or not.
# Skips without `brew` unless PTYRO_REQUIRE_BREW=1. Needs `make dist` first, like `dist-check`.
set -eu
cd "$(dirname "$0")/.."
if ! command -v brew >/dev/null 2>&1; then
  if [ "${PTYRO_REQUIRE_BREW:-0}" = 1 ]; then echo "brew-check: Homebrew is required (PTYRO_REQUIRE_BREW=1) but \`brew\` isn't installed" >&2; exit 1; fi
  echo "brew-check: skipped, Homebrew isn't installed (PTYRO_REQUIRE_BREW=1 makes that a failure)"
  exit 0
fi
export HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_CLEANUP=1 HOMEBREW_NO_ANALYTICS=1 HOMEBREW_NO_ENV_HINTS=1
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TARGET=${TARGET:-$(rustc -vV | sed -n 's/^host: //p')}
NAME=ptyro-$VERSION-$TARGET
ABS=$(pwd)
[ -f "dist/$NAME.tar.gz" ] || { echo "brew-check: dist/$NAME.tar.gz is missing: run \`make dist\` first" >&2; exit 1; }
PREFIX=$(brew --prefix)
# A brew-installed ptyro is linked into $PREFIX/bin and can shadow (and be picked up by a running
# daemon as) your own ptyro: never replace one, and never touch one that isn't ours.
if [ -e "$PREFIX/bin/ptyro" ] || brew list --formula ptyro >/dev/null 2>&1; then
  echo "brew-check: a ptyro is already installed in Homebrew ($PREFIX/bin/ptyro). This check installs and removes one, so it won't run over yours: run it on a machine or CI runner without it, or \`brew uninstall ptyro\` first." >&2
  exit 1
fi
TAP=ptyro-verify/check$$
FORMULA=$TAP/ptyro
work=$(mktemp -d)
tapped=0 installed=0
# `brew tap-new` switches Homebrew's developer mode on: put it back as it was.
dev=$(brew developer 2>&1 | grep -ci "is enabled" || true)
cleanup() {
  [ "$installed" = 1 ] && brew uninstall --force ptyro >/dev/null 2>&1
  [ "$tapped" = 1 ] && brew untap "$TAP" >/dev/null 2>&1
  [ "$dev" = 0 ] && brew developer off >/dev/null 2>&1
  rm -rf "$work"
  return 0
}
trap cleanup EXIT
trap 'exit 130' INT TERM

sh_render() { BASE_URL="file://$ABS/dist" ONLY="$TARGET" OUT="$1" scripts/homebrew.sh >/dev/null; }
brew tap-new --no-git "$TAP" >/dev/null
tapped=1
dir=$(brew --repository "$TAP")/Formula
mkdir -p "$dir"

# 1. A wrong checksum is refused, and nothing is left linked.
sh_render "$work/good.rb"
good=$(sed -n 's/^ *sha256 "\([0-9a-f]\{64\}\)"/\1/p' "$work/good.rb" | grep -v '^0*$' | head -1)
[ -n "$good" ] || { echo "brew-check: the rendered formula has no checksum for $TARGET" >&2; exit 1; }
sed "s/$good/$(printf 'f%.0s' $(seq 64))/" "$work/good.rb" > "$dir/ptyro.rb"
if brew install --formula "$FORMULA" >"$work/bad.out" 2>&1; then
  installed=1; echo "brew-check: a formula with the wrong checksum was installed" >&2; exit 1
fi
grep -qi "checksum" "$work/bad.out" || { echo "brew-check: the refusal doesn't mention the checksum" >&2; cat "$work/bad.out" >&2; exit 1; }
[ ! -e "$PREFIX/bin/ptyro" ] || { echo "brew-check: a refused install left a ptyro behind" >&2; exit 1; }

# 2. The real one installs.
cp "$work/good.rb" "$dir/ptyro.rb"
installed=1
brew install --formula "$FORMULA"
got=$("$PREFIX/bin/ptyro" --version)
[ "$got" = "ptyro $VERSION" ] || { echo "brew-check: installed binary says '$got', expected 'ptyro $VERSION'" >&2; exit 1; }
brew test "$FORMULA"
for f in etc/bash_completion.d/ptyro share/zsh/site-functions/_ptyro share/fish/vendor_completions.d/ptyro.fish; do
  [ -s "$PREFIX/$f" ] || { echo "brew-check: completion $PREFIX/$f wasn't installed" >&2; exit 1; }
done
brew info "$FORMULA" >"$work/info" 2>&1
grep -q "ptyro daemon upgrade" "$work/info" || { echo "brew-check: the caveats aren't shown" >&2; cat "$work/info" >&2; exit 1; }
grep -q "brew upgrade ptyro" "$work/info" || { echo "brew-check: the caveats don't say how to upgrade" >&2; exit 1; }

# 3. Audit. Checks that need a published release or the network (the URL being reachable, the
# GitHub repository's age and stars, livecheck) are not run: there is no release yet.
if brew audit --strict "$FORMULA" >"$work/audit" 2>&1; then
  echo "brew audit --strict: ok"
else
  cat "$work/audit" >&2; echo "brew-check: brew audit --strict found problems" >&2; exit 1
fi
echo "brew-check ok: $NAME installs from a tap, tests and audits clean, completions and caveats are there, and a wrong checksum is refused (not checked: --online audit, needs a published release)"
