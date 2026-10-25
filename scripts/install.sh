#!/bin/sh
# Install a ptyro release (ROADMAP H2.2/H2.3): download the tarball for this machine, check its
# SHA-256 against the release's SHA256SUMS (and its signed provenance when `gh` is installed),
# and put the binary in ~/.local/bin. Downloading with curl leaves no macOS quarantine mark, so
# Gatekeeper doesn't block it (a browser download does; see README).
#
#   curl -fsSL https://raw.githubusercontent.com/nealhardesty/ptyro/master/scripts/install.sh | sh
#   sh install.sh                 # the latest release
#   PTYRO_VERSION=0.0.5 sh install.sh
#
# Environment:
#   PTYRO_VERSION          the version to install, like 0.0.5 (default: the latest release)
#   PTYRO_PREFIX           where to install (default ~/.local): the binary goes in $PREFIX/bin,
#                          shell completions in $PREFIX/share/ptyro/completions
#   PTYRO_RELEASE_URL      a directory (URL, file:// too) with the tarball and SHA256SUMS
#                          instead of GitHub's release (for tests and mirrors)
#   PTYRO_VERIFY=0         don't try `gh attestation verify`
#   PTYRO_REQUIRE_ATTESTATION=1   fail unless the provenance verifies (needs `gh`)
#
# To remove it again: `ptyro uninstall`.
set -eu

REPO=nealhardesty/ptyro
say() { printf '%s\n' "$*"; }
die() { printf 'install: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

have curl || die "curl is needed"
have tar || die "tar is needed"

case "$(uname -s)" in
  Linux) os=unknown-linux-gnu ;;
  Darwin) os=apple-darwin ;;
  *) die "$(uname -s) isn't supported: ptyro runs on Linux and macOS (WSL2 works on Windows)" ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) die "$(uname -m) isn't a platform ptyro ships for" ;;
esac
triple="$arch-$os"
prefix="${PTYRO_PREFIX:-$HOME/.local}"

version="${PTYRO_VERSION:-}"
if [ -z "$version" ]; then
  [ -z "${PTYRO_RELEASE_URL:-}" ] || die "set PTYRO_VERSION with PTYRO_RELEASE_URL"
  version=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
    sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -1)
  [ -n "$version" ] || die "couldn't find the latest release (no network? a rate limit? set PTYRO_VERSION)"
fi
version="${version#v}"
base="${PTYRO_RELEASE_URL:-https://github.com/$REPO/releases/download/v$version}"
name="ptyro-$version-$triple"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
say "downloading ptyro $version for $triple"
curl -fsSL -o "$tmp/$name.tar.gz" "$base/$name.tar.gz" ||
  die "no download at $base/$name.tar.gz (is $version released for $triple?)"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || die "no checksums at $base/SHA256SUMS"

# The checksum line for our file, checked with whatever this machine has.
want=$(awk -v f="$name.tar.gz" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
[ -n "$want" ] || die "SHA256SUMS has no entry for $name.tar.gz"
if have sha256sum; then got=$(sha256sum "$tmp/$name.tar.gz" | cut -d' ' -f1)
elif have shasum; then got=$(shasum -a 256 "$tmp/$name.tar.gz" | cut -d' ' -f1)
else die "sha256sum or shasum is needed to check the download"; fi
[ "$want" = "$got" ] || die "the download's checksum is $got, the release says $want: not installing"
say "checksum ok"

# Provenance: the release's tarballs are attested by the workflow that built them.
if [ "${PTYRO_VERIFY:-1}" != 0 ] && have gh; then
  if gh attestation verify "$tmp/$name.tar.gz" --repo "$REPO" >/dev/null 2>&1; then
    say "provenance ok (built by $REPO's release workflow)"
  elif [ "${PTYRO_REQUIRE_ATTESTATION:-0}" = 1 ]; then
    die "the provenance didn't verify"
  else
    say "note: couldn't verify the provenance with gh (not logged in, or an old release); the checksum matched"
  fi
elif [ "${PTYRO_REQUIRE_ATTESTATION:-0}" = 1 ]; then
  die "PTYRO_REQUIRE_ATTESTATION=1 needs the gh CLI"
fi

tar -C "$tmp" -xzf "$tmp/$name.tar.gz"
[ -x "$tmp/$name/ptyro" ] || die "the tarball has no ptyro binary"
mkdir -p "$prefix/bin"
# Atomic: a running ptyro keeps its old file; the new one is there at the next start.
cp "$tmp/$name/ptyro" "$prefix/bin/.ptyro.new.$$"
chmod 755 "$prefix/bin/.ptyro.new.$$"
mv -f "$prefix/bin/.ptyro.new.$$" "$prefix/bin/ptyro"
if [ -d "$tmp/$name/completions" ]; then
  mkdir -p "$prefix/share/ptyro/completions"
  cp "$tmp/$name/completions/"* "$prefix/share/ptyro/completions/" 2>/dev/null || true
fi

say "installed $prefix/bin/ptyro ($("$prefix/bin/ptyro" --version 2>/dev/null || echo "ptyro $version"))"
case ":$PATH:" in
  *":$prefix/bin:"*) ;;
  *) say "add it to your PATH:  export PATH=\"$prefix/bin:\$PATH\"" ;;
esac
say "next: ptyro doctor   (checks your setup)   ·   ptyro   (opens it)"
say "if ptyro was already running: ptyro daemon upgrade   (switches the daemon to this one, sessions stay)"
say "to remove it again: ptyro uninstall"
