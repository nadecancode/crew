#!/usr/bin/env bash
# Read-only: one Claude Code session file in, one TSV row of numbers out.
# No message text leaves this script; only counts, token figures and ids.
#
# usage: session.sh <label> <session.jsonl> <cutoff-epoch-seconds>
# Records stamped after the cutoff are ignored, so a rerun over the same
# (append-only) files gives the same numbers.
#
# A request is one main-thread assistant response (sidechains and synthetic
# lines left out), counted once per message id. A big write writes at least
# BIG tokens to cache and more than it reads; each is classed by the first
# test it passes:
#   first                   the session file's first request
#   compaction              a compact_boundary since the previous request
#   model_change            another model than the previous request
#   ttl_expired             over TTL seconds since the previous request and a
#                           cache read below INTACT (the session's own system
#                           prompt was not read either)
#   history_logged          Claude Code logged a thinking_drop for this
#                           request id (client-declared history change)
#   history_unlogged        cache read at least INTACT (system prompt read,
#                           messages written again), no thinking_drop
#   system_lost_within_ttl  the rest: unexplained
# Writes below BIG are not classed. Turns end at system/turn_duration; a turn
# with a fold holds an isMeta user row whose array content starts "[crew]"
# (crew-bridge's session.append); drops are counted against the turn before.
set -euo pipefail
label=$1
file=$2
cutoff=$3

jq -r '
  def ts: .timestamp | sub("\\.[0-9]+Z$"; "Z") | fromdateiso8601;
  if .type == "assistant" and (.isSidechain | not) and .message.usage != null
     and (.message.model // "") != "<synthetic>" then
    ["R", ts, .message.id, (.requestId // ""),
     (.message.usage.input_tokens // 0),
     (.message.usage.cache_creation_input_tokens // 0),
     (.message.usage.cache_read_input_tokens // 0),
     (.message.usage.cache_creation.ephemeral_1h_input_tokens // 0),
     (.message.usage.cache_creation.ephemeral_5m_input_tokens // 0),
     (.message.model // "")] | @tsv
  elif .type == "attachment" and .attachment.type == "thinking_drop" then
    ["D", ts, (.attachment.requestId // "")] | @tsv
  elif .type == "attachment" and .attachment.type == "silent_turn_reminder" then
    ["S", ts] | @tsv
  elif .type == "system" and (.subtype // "") == "compact_boundary" then
    ["C", ts] | @tsv
  elif .type == "system" and (.subtype // "") == "turn_duration" then
    ["E", ts] | @tsv
  elif .type == "user" and (.isSidechain | not) and (.isMeta == true)
       and (.message.content | type) == "array"
       and ((.message.content[0].text // "") | startswith("[crew]")) then
    ["F", ts] | @tsv
  else empty end' "$file" |
awk -F'\t' -v label="$label" -v cutoff="$cutoff" -v BIG=10000 -v INTACT=26000 -v TTL=3600 '
  # Pass over records in file order; requests are kept once per message id.
  $2 > cutoff { next }
  { n++; kind[n] = $1; t[n] = $2
    if ($1 == "R") { mid[n] = $3; rid[n] = $4; inp[n] = $5; cc[n] = $6; cr[n] = $7
                     c1h[n] = $8; c5m[n] = $9; model[n] = $10 }
    if ($1 == "D" && $3 != "") dropped[$3] = 1 }
  END {
    OFS = "\t"
    for (i = 1; i <= n; i++) {
      if (kind[i] == "C") { compact = 1; continue }
      if (kind[i] == "F") { fold = 1; continue }
      if (kind[i] == "S") { silent = 1; continue }
      if (kind[i] == "E") {
        turns++; last = fold ? "fold" : (silent ? "nofold_silent" : "nofold_quiet")
        tc[last]++; fold = 0; silent = 0; continue
      }
      if (kind[i] == "D") { drops++; if (last != "") dc[last]++; continue }
      # A request (once per message id).
      if (seen[mid[i]]++) continue
      reqs++; tin += inp[i]; tcc += cc[i]; tcr += cr[i]; t1h += c1h[i]; t5m += c5m[i]
      m = model[i]; mr[m]++; min_[m] += inp[i]; m1h[m] += c1h[i]; m5m[m] += c5m[i]; mcr[m] += cr[i]
      gap = (prev == "") ? -1 : t[i] - prev
      if (cc[i] >= BIG && cc[i] > cr[i]) {
        if (prev == "") c = "first"
        else if (compact) c = "compaction"
        else if (model[i] != pmodel) c = "model_change"
        else if (gap > TTL && cr[i] < INTACT) c = "ttl_expired"
        else if (rid[i] in dropped) c = "history_logged"
        else if (cr[i] >= INTACT) c = "history_unlogged"
        else c = "system_lost_within_ttl"
        bn[c]++; bt[c] += cc[i]
        print "SIG", label, c, cr[i], cc[i], gap, model[i] > "/dev/stderr"
      }
      compact = 0; prev = t[i]; pmodel = model[i]
    }
    for (m in mr) print "MOD", label, m, mr[m], min_[m], m1h[m], m5m[m], mcr[m] > "/dev/stderr"
    split("first compaction model_change ttl_expired history_logged history_unlogged system_lost_within_ttl", cls, " ")
    row = label OFS reqs OFS tin OFS tcc OFS tcr OFS t1h OFS t5m OFS drops+0
    for (k = 1; k <= 7; k++) row = row OFS bn[cls[k]]+0 OFS bt[cls[k]]+0
    row = row OFS tc["fold"]+0 OFS dc["fold"]+0
    row = row OFS tc["nofold_quiet"]+tc["nofold_silent"] OFS dc["nofold_quiet"]+dc["nofold_silent"]
    row = row OFS tc["nofold_silent"]+0 OFS dc["nofold_silent"]+0
    if (reqs > 0) print row
  }'
