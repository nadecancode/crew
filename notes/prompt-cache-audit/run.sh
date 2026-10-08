#!/usr/bin/env bash
# Read-only measurement behind notes/prompt-cache.md §Audit "Measured sample".
#
# Cohort: every session file of a crew member worktree
# (~/.claude/projects/<workspace>--crew-worktrees-*/*.jsonl), plus each
# session file of the workspace itself whose first plain-text user prompt
# starts with "[crew]" (the orchestrator and members launched there).
#
# usage: run.sh [projects-dir] [workspace-dir-name]
# CUTOFF (epoch seconds, default 1791432000 = 2026-10-08T04:00:00Z) bounds
# the records read; sessions with no request before it are left out.
# Writes sessions.tsv (one row per session), signatures.tsv (class, cache
# read, cache write and gap of every big write) and totals.tsv next to itself.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
projects=${1:-$HOME/.claude/projects}
workspace=${2:--home-nade-workspaces-personal-crew}
cutoff=${CUTOFF:-1791432000}

{
  for f in "$projects/$workspace"--crew-worktrees-*/*.jsonl; do
    [ -s "$f" ] || continue
    dir=${f%/*}; agent=${dir##*--crew-worktrees-}; id=${f##*/}
    printf '%s\t%s\n' "$agent/${id:0:8}" "$f"
  done
  for f in "$projects/$workspace"/*.jsonl; do
    [ -s "$f" ] || continue
    first=$(jq -n -r 'first(inputs | select(.type == "user" and (.message.content | type) == "string") | .message.content) // ""' "$f")
    case $first in "[crew]"*) id=${f##*/}; printf '%s\t%s\n' "root/${id:0:8}" "$f" ;; esac
  done
} > "$here/cohort.tmp"

header="session\trequests\tinput\tcache_write\tcache_read\twrite_1h\twrite_5m\tthinking_drops"
for c in first compaction model_change ttl_expired history_logged history_unlogged system_lost_within_ttl; do
  header="$header\t${c}_n\t${c}_tokens"
done
header="$header\tfold_turns\tdrops_after_fold\tnofold_turns\tdrops_after_nofold\tnofold_silent_turns\tdrops_after_nofold_silent"
printf '%b\n' "$header" > "$here/sessions.tsv"
: > "$here/signatures.tsv"
while IFS=$'\t' read -r label file; do
  "$here/session.sh" "$label" "$file" "$cutoff" >> "$here/sessions.tsv" 2>> "$here/signatures.tsv"
done < "$here/cohort.tmp"
rm -f "$here/cohort.tmp"

awk -F'\t' 'NR == 1 { for (i = 2; i <= NF; i++) name[i] = $i; next }
  { rows++; for (i = 2; i <= NF; i++) sum[i] += $i }
  END { printf "sessions\t%d\n", rows; for (i = 2; i <= length(name) + 1; i++) printf "%s\t%d\n", name[i], sum[i] }' \
  "$here/sessions.tsv" > "$here/totals.tsv"
cat "$here/totals.tsv"
