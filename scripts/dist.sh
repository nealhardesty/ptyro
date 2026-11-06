#!/bin/sh
# Release tarball for one platform (ROADMAP M8.8): dist/ptyro-<version>-<target>.tar.gz with the
# binary, shell completions, README, LICENSE and CHANGELOG, plus its .sha256. Windows gets a .zip
# instead, with ptyro.exe, ptyrow.exe (its windowless launcher, WIN10), PowerShell's completions (WIN3; built on Windows, in Git Bash) and the
# packaged ConPTY its sessions run in (conpty.dll, OpenConsole.exe: WIN5, `make conpty`). Run
# via `make dist` (TRIPLE=<rust target triple> to cross-build; default: this machine).
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
case "$TARGET" in *-windows-*) EXE=.exe ARCHIVE=zip ;; *) EXE= ARCHIVE=tar.gz ;; esac
BIN=target/$TARGET/release/ptyro$EXE
NAME=ptyro-$VERSION-$TARGET
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/$NAME/completions" dist
cp "$BIN" README.md LICENSE "$STAGE/$NAME/"
# Windows: ptyrow.exe too, which starts ptyro.exe without a window (the logon task, a toast's
# click: WIN10).
case "$TARGET" in *-windows-*) cp "target/$TARGET/release/ptyrow.exe" "$STAGE/$NAME/" ;; esac
case "$TARGET" in
  aarch64-pc-windows-*) INTO="$STAGE/$NAME" ARCH=aarch64 sh scripts/conpty.sh ;;
  *-pc-windows-*) INTO="$STAGE/$NAME" ARCH=x86_64 sh scripts/conpty.sh ;;
esac
[ -f CHANGELOG.md ] && cp CHANGELOG.md "$STAGE/$NAME/"
# Completions from the binary itself, when it runs here (not when cross-built).
if "$BIN" --version >/dev/null 2>&1; then
  if [ "$ARCHIVE" = zip ]; then
    "$BIN" completions powershell > "$STAGE/$NAME/completions/ptyro.ps1"
  else
    "$BIN" completions bash > "$STAGE/$NAME/completions/ptyro.bash"
    "$BIN" completions zsh > "$STAGE/$NAME/completions/_ptyro"
    "$BIN" completions fish > "$STAGE/$NAME/completions/ptyro.fish"
  fi
fi
rm -f "dist/$NAME.$ARCHIVE"
if [ "$ARCHIVE" = tar.gz ]; then
  tar -C "$STAGE" -czf "dist/$NAME.tar.gz" "$NAME"
elif command -v zip >/dev/null; then
  (cd "$STAGE" && zip -qr - "$NAME") > "dist/$NAME.zip"
else
  # Git Bash has no zip; Windows' own PowerShell does, given Windows paths.
  powershell.exe -NoProfile -Command "Compress-Archive -Path '$(cygpath -w "$STAGE/$NAME")' -DestinationPath '$(cygpath -w "$PWD/dist/$NAME.zip")'"
fi
sum() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
(cd dist && sum "$NAME.$ARCHIVE" > "$NAME.$ARCHIVE.sha256")
echo "dist/$NAME.$ARCHIVE"
cat "dist/$NAME.$ARCHIVE.sha256"
