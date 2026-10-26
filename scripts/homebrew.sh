#!/bin/sh
# Render the Homebrew formula (ROADMAP M8.8) from packaging/homebrew/ptyro.rb.in and the
# checksums in dist/ (from `make dist` on each platform, or the release's SHA256SUMS).
# Writes dist/ptyro.rb; fails if a platform's checksum is missing. Run via `make homebrew-formula`.
# For `make brew-check` only: BASE_URL=<url dir> replaces the GitHub release download URL, and
# ONLY=<triple> renders just that platform (the others get a zero checksum, never fetched).
set -eu
cd "$(dirname "$0")/.."
VERSION=${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}
cat dist/*.sha256 dist/SHA256SUMS 2>/dev/null | sort -u > dist/.sums || true
OUT=${OUT:-dist/ptyro.rb}
out=$(sed "s/@VERSION@/$VERSION/g" packaging/homebrew/ptyro.rb.in)
if [ -n "${BASE_URL:-}" ]; then
  out=$(printf '%s\n' "$out" | sed "s|https://github.com/nealhardesty/ptyro/releases/download/v$VERSION|$BASE_URL|")
fi
for t in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  if [ -n "${ONLY:-}" ] && [ "$t" != "$ONLY" ]; then
    out=$(printf '%s\n' "$out" | sed "s/@SHA_$t@/0000000000000000000000000000000000000000000000000000000000000000/")
    continue
  fi
  sha=$(awk -v f="ptyro-$VERSION-$t.tar.gz" '$2 == f || $2 == "*" f {print $1; exit}' dist/.sums)
  [ -n "$sha" ] || { echo "homebrew: no checksum for ptyro-$VERSION-$t.tar.gz in dist/" >&2; exit 1; }
  out=$(printf '%s\n' "$out" | sed "s/@SHA_$t@/$sha/")
done
rm -f dist/.sums
printf '%s\n' "$out" > "$OUT"
echo "$OUT"
