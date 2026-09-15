#!/bin/sh
set -eu

results=${1:-calibration/results}
output=${2:-calibration/analysis.json}
command -v jq >/dev/null || { printf '%s\n' 'jq is required' >&2; exit 2; }
files=$(find "$results" -mindepth 2 -maxdepth 2 -name commits.jsonl -print | sort)
test -n "$files" || { printf 'no commit result files in %s\n' "$results" >&2; exit 2; }
tmp=$(mktemp "${TMPDIR:-/tmp}/slop-gate-analysis.XXXXXX")
trap 'rm -f "$tmp"' EXIT INT TERM
for file in $files; do
    jq -c --arg file "$file" 'select(type == "object") + {source: $file}' "$file" >> "$tmp"
done

jq -s '
  def ok: select(.status == "ok" and (.erosion_delta | type) == "number");
  def quantile($p): sort | if length == 0 then null else .[((length - 1) * $p | floor)] end;
  {samples: length,
   valid_samples: ([.[] | ok] | length),
   invalid_samples: ([.[] | select(.status != "ok")] | length),
   repositories: ([.[].repository] | unique | length),
   valid_repositories: ([.[] | ok | .repository] | unique | length),
   delta: ([.[] | ok | .erosion_delta] | {minimum: quantile(0), median: quantile(0.5),
     p90: quantile(0.9), p95: quantile(0.95), p99: quantile(0.99), maximum: quantile(1),
     at_0_08: (map(select(. >= 0.08)) | length)}),
   by_repository: ([.[] | ok] | group_by(.repository) | map({repository: .[0].repository,
     samples: length, median_delta: ([.[].erosion_delta] | quantile(0.5)),
     p95_delta: ([.[].erosion_delta] | quantile(0.95))}))}
' "$tmp" > "$output"
jq empty "$output"
