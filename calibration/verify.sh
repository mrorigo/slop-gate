#!/bin/sh
set -eu

usage() { printf '%s\n' 'usage: calibration/verify.sh RESULTS [SECOND_RESULTS]'; }
results=${1:-}
[ -n "$results" ] || { usage >&2; exit 2; }
command -v jq >/dev/null || { printf '%s\n' 'jq is required' >&2; exit 2; }
repos=$(find "$results" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')
[ "$repos" -eq 20 ] || { printf 'expected 20 repositories, found %s\n' "$repos" >&2; exit 1; }
valid=0
for file in "$results"/*/commits.jsonl; do
    test -f "$file" || { printf 'missing commit records: %s\n' "$file" >&2; exit 1; }
    while IFS= read -r record; do
        printf '%s\n' "$record" | jq -e 'type == "object" and (.repository | type) == "string" and (.commit | type) == "string" and (.parent | type) == "string" and (.date | type) == "string" and (.status == "ok" and (.head | type) == "object" and (.base | type) == "object" and (.erosion_delta | type) == "number")' >/dev/null
        valid=$((valid + 1))
    done < "$file"
done
[ "$valid" -ge 200 ] || { printf 'expected at least 200 valid records, found %s\n' "$valid" >&2; exit 1; }

if [ "$#" -ge 2 ]; then
    second=$2
    first_normalized=$(mktemp "${TMPDIR:-/tmp}/slop-gate-first.XXXXXX")
    second_normalized=$(mktemp "${TMPDIR:-/tmp}/slop-gate-second.XXXXXX")
    trap 'rm -f "$first_normalized" "$second_normalized"' EXIT INT TERM
    find "$results" -type f -name '*.json*' -print | sort | while read -r file; do
        relative=${file#"$results"/}
        jq -S -c . "$file" | sed "s#^#$relative\t#"
    done > "$first_normalized"
    find "$second" -type f -name '*.json*' -print | sort | while read -r file; do
        relative=${file#"$second"/}
        jq -S -c . "$file" | sed "s#^#$relative\t#"
    done > "$second_normalized"
    cmp -s "$first_normalized" "$second_normalized" || { printf '%s\n' 'normalized results differ' >&2; exit 1; }
fi
printf 'verified %s repositories and %s valid commit records\n' "$repos" "$valid"
