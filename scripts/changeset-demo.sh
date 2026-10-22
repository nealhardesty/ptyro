#!/bin/sh
# M11 change sets, end to end, with no real agents: a throwaway ptyro (target/changeset-demo)
# whose claude and codex are `ptyro fake-agent`, three repositories with bare "remotes" (api,
# sdk, web: npm packages that depend on each other), and one change set across them. It shows
# the order read from the manifests, the overlay, the lead's contract and its approval, write
# fences, blocked/ready wake-ups, a contract revision reaching exactly the member it affects, a
# set checkpoint restored across repositories, linked commits, and the merge train.
#
#   make changeset-demo           run it (about 30 s), then leave the daemon up to explore
#   make changeset-demo ARGS=tui  …and open the TUI on it (Ctrl-Q S: the set view)
#   make changeset-demo ARGS=clean
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
dir="$root/target/changeset-demo"
bin="$root/target/debug/ptyro"
export PTYRO_HOME="$dir/ptyro" PTYRO_BIN="$bin" HOME="$dir/home" GIT_CONFIG_NOSYSTEM=1 SHELL=/bin/sh
unset PTYRO_SOCKET PTYRO_SESSION_ID PTYRO_CONFIG PTYRO_CS
export PTYRO_ALLOW_NESTED=1
p() { "$bin" "$@"; }
say() { printf '\n\033[1;36m▸ %s\033[0m\n' "$*"; }
until_() { # seconds, then a shell condition
  n=$(($1 * 4)); shift
  while [ $n -gt 0 ]; do
    if sh -c "$*" >/dev/null 2>&1; then return 0; fi
    sleep 0.25; n=$((n - 1))
  done
  echo "timed out waiting for: $*" >&2; return 1
}
show() { p cs show tier --json | python3 -c "import json,sys; i=json.load(sys.stdin); print($1)"; }

if [ "${1:-}" = clean ]; then
  p daemon stop --force >/dev/null 2>&1 || true
  rm -rf "$dir"
  echo "changeset-demo removed"
  exit 0
fi
if [ "${1:-}" = tui ] && [ -d "$dir/api" ]; then
  cd "$dir" && exec "$bin" attach tier-lead
fi

p daemon stop --force >/dev/null 2>&1 || true
rm -rf "$dir"
mkdir -p "$HOME" "$PTYRO_HOME/config"
printf '[user]\n\tname = Demo\n\temail = demo@example.invalid\n[init]\n\tdefaultBranch = main\n' > "$HOME/.gitconfig"
{
  printf '[attention]\nmin_work = "1s"\n[worktrees]\nfetch = false\n'
  for a in claude codex; do
    printf '[agents.%s]\ncommand = ["%s", "fake-agent", "--as", "%s"]\n' "$a" "$bin" "$a"
  done
} > "$PTYRO_HOME/config/config.toml"
repo() { # name package.json
  git init -q --bare "$dir/$1.git"
  git clone -q "$dir/$1.git" "$dir/$1" 2>/dev/null
  printf '# %s\n' "$1" > "$dir/$1/README.md"
  printf '%s\n' "$2" > "$dir/$1/package.json"
  (cd "$dir/$1" && git add -A && git commit -qm init && git push -q origin main)
  p project add "$dir/$1" >/dev/null
}
repo api '{"name":"@acme/api"}'
repo sdk '{"name":"@acme/sdk","dependencies":{"@acme/api":"1"}}'
repo web '{"name":"web","dependencies":{"@acme/api":"1","@acme/sdk":"1"}}'
cd "$dir"

say "ptyro cs new tier --projects api,sdk,web --agent api=codex --overlay   (order from package.json)"
p cs new tier --projects api,sdk,web --agent api=codex --overlay -p "users get a tier: free or pro"
set_root=$(show "i['set']['root']")
echo "web builds against api's worktree: node_modules/@acme/api → $(readlink "$set_root/web/node_modules/@acme/api")"

say "the lead drafts CONTRACT.md (here: written for it), and its fence keeps it out of members' code"
until_ 20 "'$bin' read tier-lead | grep -q 'fake claude'"
p send tier-lead "edit api/hack.txt nope" >/dev/null
until_ 20 "'$bin' read tier-lead | grep -q 'may not edit api/hack.txt'"
p read tier-lead | grep 'may not edit' | head -1
cat > "$set_root/CONTRACT.md" <<'EOF'
# Contract: tier

## api interface
affects: api, sdk, web

GET /users/{id} adds "tier": "free" | "pro".

## sdk interface
affects: sdk

IsPro(user) bool

## Merge order
1. api  2. sdk  3. web
EOF
p send tier-lead "edit notes.md drafted" >/dev/null
until_ 20 "'$bin' gate ls | grep -q contract-r1"
p cs status tier

say "ptyro cs contract tier approve   (the member agents start, each with its part)"
p cs contract tier approve
until_ 20 "test \$('$bin' cs show tier --json | python3 -c 'import json,sys; print(len(json.load(sys.stdin)[\"sessions\"]))') = 3"
until_ 20 "'$bin' read tier-sdk | grep -q 'IsPro'"

say "sdk is blocked on api; api says ready; sdk is woken"
p send tier-sdk 'sh "$PTYRO_BIN" cs blocked "$PTYRO_CS" api need-the-field' >/dev/null
until_ 20 "'$bin' cs show tier --json | grep -q 'blocked_on:api'"
p send tier-api 'edit tier.txt free|pro' >/dev/null
p send tier-api 'sh "$PTYRO_BIN" cs ready "$PTYRO_CS" field-is-in' >/dev/null
until_ 20 "'$bin' read tier-sdk | grep -q 'kind=ready_for_integration'"
echo "sdk got: $(p read tier-sdk | grep -m1 'is ready for integration' | sed 's/^ *//' | cut -c1-90)"

say "a revision of the sdk section reaches sdk at once (interrupt), the others later"
sed 's/IsPro(user) bool/IsPro(user) bool; IsFree(user) bool/' "$set_root/CONTRACT.md" > "$dir/r2.md"
p cs contract tier revise "$dir/r2.md" -m "add IsFree"

say "a set checkpoint, a bad turn in two repositories, and one restore"
printf 'IsPro\n' > "$set_root/sdk/sdk.txt"
printf 'PRO badge\n' > "$set_root/web/web.txt"
n=$(p cs checkpoint tier good 2>&1 | sed -n 's/.*#\([0-9]*\).*/\1/p')
rm "$set_root/api/tier.txt"; printf 'broken\n' > "$set_root/web/web.txt"
p cs restore tier "$n" --force
echo "api/tier.txt: $(cat "$set_root/api/tier.txt"), web/web.txt: $(cat "$set_root/web/web.txt")"

say "ptyro cs commit tier -m …   (linked by Change-Set trailers)"
p cs commit tier -m "Add user tier"
git -C "$set_root/web" log -1 --format=%B | sed -n '/Change-Set/p'

say "ptyro cs land tier --yes   (the merge train: api → sdk → web)"
p cs land tier --yes
for r in api sdk web; do echo "$r main: $(git -C "$dir/$r.git" log -1 --format=%s main)"; done

say "ptyro cs log tier   (the ledger)"
p cs log tier | tail -n 20
printf '\nThe daemon is still up: PTYRO_HOME=%s %s cs show tier\n' "$PTYRO_HOME" "$bin"
echo "make changeset-demo ARGS=tui opens the TUI (Ctrl-Q S); ARGS=clean removes it all"
