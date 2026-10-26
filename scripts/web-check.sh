#!/bin/sh
# The committed web app (web/dist, embedded in the binary by `ptyro serve`) must match its
# TypeScript sources: rebuild into a scratch copy and compare. Run via `make web-check` (CI).
set -eu
cd "$(dirname "$0")/.."
TSC=${TSC:-npx -y -p typescript@5.9.3 tsc}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cp -R web "$tmp/web"
(cd "$tmp/web" && $TSC -p tsconfig.app.json && $TSC -p tsconfig.sw.json)
status=0
for f in $(cd "$tmp/web/dist" && ls *.js); do
  if ! cmp -s "web/dist/$f" "$tmp/web/dist/$f"; then
    echo "web/dist/$f is out of date: run \`make web\` and commit the result" >&2
    status=1
  fi
done
[ $status -eq 0 ] && echo "web/dist matches web/src"
exit $status
