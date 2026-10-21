#!/bin/sh
# Changelog automation (ROADMAP M8.8). Prints a CHANGELOG.md built from git history: one section
# per `v*` tag (newest first) and "Unreleased" for commits after the last one. Each commit's
# subject is an entry, grouped by its prefix (`M7:` milestones, `fix:`, `perf:`, `docs:`, …);
# a milestone commit's body bullets are kept, since they say what shipped.
#   make changelog            rewrite CHANGELOG.md
#   scripts/changelog.sh --notes v0.1.0   just that release's section (GitHub release notes)
set -eu
cd "$(dirname "$0")/.."
notes=${2:-}
[ "${1:-}" = "--notes" ] || notes=

section() { # TITLE RANGE
  title=$1 range=$2
  log=$(git log --no-merges --format='%H' $range)
  [ -n "$log" ] || return 0
  printf '## %s\n\n' "$title"
  for kind in milestone feat fix perf research docs other; do
    out=$(for h in $log; do
      s=$(git log -1 --format=%s "$h")
      case $s in
        M[0-9]*:*) k=milestone;; feat*) k=feat;; fix*) k=fix;; perf*) k=perf;; research*) k=research;;
        docs*|style*) k=docs;; *) k=other;;
      esac
      [ "$k" = "$kind" ] || continue
      printf -- '- %s (%s)\n' "$s" "$(git log -1 --format=%h "$h")"
      # Milestone bodies: keep their top-level bullets.
      [ "$k" = milestone ] || continue
      git log -1 --format=%b "$h" | awk '
        /^[-*] / { if (b != "") print b; b = "  - " substr($0, 3); next }
        /^  +[^ ]/ && b != "" { sub(/^ +/, ""); b = b " " $0; next }
        { if (b != "") print b; b = "" }
        END { if (b != "") print b }'
    done)
    [ -n "$out" ] || continue
    case $kind in
      milestone) h=Milestones;; feat) h=Features;; fix) h=Fixes;; perf) h=Performance;;
      research) h=Research;; docs) h=Documentation;; *) h=Other;;
    esac
    printf '### %s\n\n%s\n\n' "$h" "$out"
  done
}

tags=$(git tag --list 'v*' --sort=-version:refname)
if [ -n "$notes" ]; then
  prev=$(printf '%s\n' $tags | sed -n "/^$notes\$/{n;p;}")
  section "$notes" "${prev:+$prev..}$notes" | sed '1,2d'
  exit 0
fi
echo "# Changelog"
echo
echo "Generated from git history by \`make changelog\` (scripts/changelog.sh); edit commit messages, not this file."
echo
last=$(printf '%s\n' $tags | head -1)
section Unreleased "${last:+$last..}HEAD"
set -- $tags
while [ $# -gt 0 ]; do
  t=$1; shift
  section "$t ($(git log -1 --format=%ad --date=short "$t"))" "${1:+$1..}$t"
done
