#!/bin/sh
# Release tarball for one platform (ROADMAP M8.8): dist/ptyro-<version>-<target>.tar.gz with the
# binary, shell completions, README, LICENSE and CHANGELOG, plus its .sha256. Run via
# `make dist` (TRIPLE=<rust target triple> to cross-build; default: this machine).
# `make dist` on a tag checks that the tag matches the crate version.
set -eu
cd "$(dirname "$0")/.."
CARGO=${CARGO:-cargo}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TARGET=${TARGET:-$(rustc -vV | sed -n 's/^host: //p')}
if tag=$(git describe --exact-match --tags 2>/dev/null) && [ "$tag" != "v$VERSION" ]; then
  echo "dist: tag $tag doesn't match the crate version $VERSION (Cargo.toml [workspace.package])" >&2
  exit 1
fi
rustup target add "$TARGET" >/dev/null 2>&1 || true
$CARGO build --release --locked -p ptyro --target "$TARGET"
BIN=target/$TARGET/release/ptyro
NAME=ptyro-$VERSION-$TARGET
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/$NAME/completions" dist
cp "$BIN" README.md LICENSE "$STAGE/$NAME/"
[ -f CHANGELOG.md ] && cp CHANGELOG.md "$STAGE/$NAME/"
# Completions from the binary itself, when it runs here (not when cross-built).
if "$BIN" --version >/dev/null 2>&1; then
  "$BIN" completions bash > "$STAGE/$NAME/completions/ptyro.bash"
  "$BIN" completions zsh > "$STAGE/$NAME/completions/_ptyro"
  "$BIN" completions fish > "$STAGE/$NAME/completions/ptyro.fish"
fi
tar -C "$STAGE" -czf "dist/$NAME.tar.gz" "$NAME"
sum() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
(cd dist && sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
echo "dist/$NAME.tar.gz"
cat "dist/$NAME.tar.gz.sha256"
