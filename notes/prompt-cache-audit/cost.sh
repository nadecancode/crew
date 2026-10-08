#!/usr/bin/env bash
# List-price input-side cost of the sample run.sh measured, per model, from
# prices.tsv (USD per million tokens, Anthropic's prompt-caching table).
# Output tokens are not counted. These are API list prices: what a Claude
# subscription is charged is not modelled.
#
# usage: cost.sh   (after run.sh; writes costs.tsv next to itself)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
awk -F'\t' -v OFS='\t' '
  FILENAME ~ /prices/ { if (FNR > 1) { base[$1] = $2; w5[$1] = $3; w1[$1] = $4; rd[$1] = $5 }; next }
  FILENAME ~ /models/ {
    m = $2
    if (!(m in base)) { unpriced[m] += $3; next }
    cin += $4 * base[m]; cw += $5 * w1[m] + $6 * w5[m]; cr += $7 * rd[m]; next
  }
  FILENAME ~ /signatures/ {
    # Every big write in the sample is a 1 h write (write_5m is 0 in totals.tsv).
    m = $6; n[$2]++; wr[$2] += $4 * w1[m]; ex[$2] += $4 * (w1[m] - rd[m])
  }
  END {
    total = cin + cw + cr
    print "item", "events", "usd", "share_of_input_side", "usd_above_reading_the_same_tokens"
    printf "%s\t\t%.2f\t%.4f\t\n", "uncached_input", cin / 1e6, cin / total
    printf "%s\t\t%.2f\t%.4f\t\n", "cache_write", cw / 1e6, cw / total
    printf "%s\t\t%.2f\t%.4f\t\n", "cache_read", cr / 1e6, cr / total
    for (c in n) printf "class:%s\t%d\t%.2f\t%.4f\t%.2f\n", c, n[c], wr[c] / 1e6, wr[c] / total, ex[c] / 1e6
    for (m in base) printf "keepalive_breakeven:%s\t\t\t\t%.0f\n", m, (w1[m] - rd[m]) / rd[m]
    for (m in unpriced) printf "unpriced_requests:%s\t%d\t\t\t\n", m, unpriced[m]
  }' "$here/prices.tsv" "$here/models.tsv" "$here/signatures.tsv" > "$here/costs.tsv"
cat "$here/costs.tsv"
