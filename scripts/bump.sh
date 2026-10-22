#!/bin/sh
# Bump the workspace version (Cargo.toml [workspace.package], which every crate inherits) and
# Cargo.lock. Run via `make bump V=patch|minor|major|X.Y.Z`. It doesn't commit or tag: the
# release workflow (README, Development) starts from the `vX.Y.Z` tag you push.
set -eu
cd "$(dirname "$0")/.."
CARGO=${CARGO:-cargo}
old=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
IFS=. read -r major minor patch <<EOF
$old
EOF
case ${1:-patch} in
  major) new=$((major + 1)).0.0 ;;
  minor) new=$major.$((minor + 1)).0 ;;
  patch) new=$major.$minor.$((patch + 1)) ;;
  [0-9]*.[0-9]*.[0-9]*) new=$1 ;;
  *) echo "bump: V must be patch, minor, major or X.Y.Z, not '$1'" >&2; exit 2 ;;
esac
if git rev-parse -q --verify "refs/tags/v$new" >/dev/null; then
  echo "bump: tag v$new already exists" >&2
  exit 1
fi
# Only the first `version = ` line: [workspace.package]'s (dist.sh and homebrew.sh read the same one).
awk -v new="$new" '!done && /^version = "/ { $0 = "version = \"" new "\""; done = 1 } { print }' \
  Cargo.toml > Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml
$CARGO update --workspace --offline --quiet
echo "$old -> $new"
echo "next: git commit -am 'release: v$new' && git tag v$new && git push origin HEAD v$new"
