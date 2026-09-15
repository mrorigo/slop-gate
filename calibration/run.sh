#!/bin/sh
set -eu

usage() { printf '%s\n' 'usage: calibration/run.sh --binary PATH [--output PATH] [--cutoffs LIST] [--keep-checkouts]'; }
binary=''
output='calibration/results'
cutoffs='10'
keep_checkouts=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary) binary=$2; shift 2 ;;
        --output) output=$2; shift 2 ;;
        --cutoffs) cutoffs=$2; shift 2 ;;
        --keep-checkouts) keep_checkouts=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done
[ -n "$binary" ] || { usage >&2; exit 2; }
command -v git >/dev/null || exit 2
command -v jq >/dev/null || { printf '%s\n' 'jq is required' >&2; exit 2; }
[ -x "$binary" ] || { printf 'binary is not executable: %s\n' "$binary" >&2; exit 2; }
binary=$(CDPATH= cd -- "$(dirname -- "$binary")" && pwd)/$(basename -- "$binary")

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
manifest="$root/calibration/manifest.toml"
case "$output" in
    /*) ;;
    *) output="$root/$output" ;;
esac
mkdir -p "$output"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/slop-gate-calibration.XXXXXX")
cleanup() { [ "$keep_checkouts" -eq 1 ] || rm -rf "$tmp"; }
trap cleanup EXIT INT TERM

version=$($binary --version)
start=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
{
    printf 'version = "%s"\n' "$(printf '%s' "$version" | sed 's/"/\\"/g')"
    printf 'started_at = "%s"\ncutoff = 10\n' "$start"
} > "$output/manifest.lock"

awk '/^name = / { name=$3; gsub(/"/, "", name) } /^url = / { url=$3; gsub(/"/, "", url); print name "\t" url }' "$manifest" |
while IFS='	' read -r name url; do
    checkout="$tmp/$name"
    repo_output="$output/$name"
    mkdir -p "$repo_output"
    if ! git clone --quiet "$url" "$checkout" 2>"$repo_output/stderr.log"; then
        printf 'clone failed\n' >> "$repo_output/stderr.log"
        printf '1\n' > "$repo_output/status"
        continue
    fi
    branch=$(git -C "$checkout" symbolic-ref --short HEAD)
    head=$(git -C "$checkout" rev-parse HEAD)
    printf 'repository = "%s"\nbranch = "%s"\nhead = "%s"\n' "$name" "$branch" "$head" >> "$output/manifest.lock"
    if ! (cd "$checkout" && "$binary" history --ref "$head" --count 20 --complexity-cutoffs "$cutoffs" --format json > "$repo_output/history.json" 2>>"$repo_output/stderr.log") || ! jq -e 'type == "array" and length > 0' "$repo_output/history.json" >/dev/null; then
        printf '1\n' > "$repo_output/status"
    else
        printf '0\n' > "$repo_output/status"
    fi
    : > "$repo_output/commits.jsonl"
    metadata="$repo_output/.metadata.json"
    git -C "$checkout" log --first-parent --format='%H%x09%P%x09%cI' -n 20 |
        awk -F '	' '{ printf "{\"commit\":\"%s\",\"parent\":\"%s\",\"date\":\"%s\"}\n", $1, $2, $3 }' |
        jq -s 'reverse | to_entries | map(.value + {index: .key})' > "$metadata"
    window="$repo_output/history.json"
    if jq -e 'type == "array" and length == 20' "$window" >/dev/null; then
        jq -n -c --arg repository "$name" --slurpfile summaries "$window" --slurpfile metadata "$metadata" \
            '$metadata[0] as $m | $summaries[0] as $s | [$m[] | select(.parent != "") | . as $entry | ($s[.index] // null) as $head | ($s[.index - 1] // null) as $base | {repository: $repository, commit: .commit, parent: .parent, date: .date, status: (if $head == null then "analysis-error" else "ok" end), head: $head, base: $base, erosion_delta: (if $head != null and $base != null then $head.erosion_ratio - $base.erosion_ratio else null end)}]' \
            | jq -c '.[]' >> "$repo_output/commits.jsonl"
    else
        printf '%s\n' '{"status":"analysis-error","reason":"fewer than 20 valid history summaries"}' >> "$repo_output/commits.jsonl"
    fi
    rm -f "$metadata"
done
failed=$(find "$output" -type f -name status -exec grep -l '^1$' {} + | wc -l | tr -d ' ')
[ "$failed" -eq 0 ]
