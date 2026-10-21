#!/bin/sh
# Render the Homebrew formula (ROADMAP M8.8) from packaging/homebrew/ptyro.rb.in and the
# checksums in dist/ (from `make dist` on each platform, or the release's SHA256SUMS).
# Writes dist/ptyro.rb; fails if a platform's checksum is missing. Run via `make homebrew-formula`.
set -eu
cd "$(dirname "$0")/.."
VERSION=${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}
cat dist/*.sha256 dist/SHA256SUMS 2>/dev/null | sort -u > dist/.sums || true
out=$(sed "s/@VERSION@/$VERSION/g" packaging/homebrew/ptyro.rb.in)
for t in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  sha=$(awk -v f="ptyro-$VERSION-$t.tar.gz" '$2 == f || $2 == "*" f {print $1; exit}' dist/.sums)
  [ -n "$sha" ] || { echo "homebrew: no checksum for ptyro-$VERSION-$t.tar.gz in dist/" >&2; exit 1; }
  out=$(printf '%s\n' "$out" | sed "s/@SHA_$t@/$sha/")
done
rm -f dist/.sums
printf '%s\n' "$out" > dist/ptyro.rb
echo dist/ptyro.rb
