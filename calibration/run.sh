#!/bin/sh
set -eu

usage() { printf '%s\n' 'usage: calibration/run.sh --binary PATH [--output PATH] [--keep-checkouts]'; }
binary=''
output='calibration/results'
keep_checkouts=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary) binary=$2; shift 2 ;;
        --output) output=$2; shift 2 ;;
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
    if ! (cd "$checkout" && "$binary" history --ref "$head" --count 12 --format json > "$repo_output/history.json" 2>>"$repo_output/stderr.log") || ! jq -e 'type == "array" and length > 0' "$repo_output/history.json" >/dev/null; then
        printf '1\n' > "$repo_output/status"
    else
        printf '0\n' > "$repo_output/status"
    fi
    : > "$repo_output/commits.jsonl"
    git -C "$checkout" log --first-parent --format='%H%x09%P%x09%cI' -n 1200 |
        awk 'NR % 60 == 1 { print }' |
        while IFS='	' read -r commit parent date; do
            test -n "$parent" || continue
            window="$repo_output/.window.json"
            if (cd "$checkout" && "$binary" history --ref "$commit" --count 2 --format json > "$window" 2>>"$repo_output/stderr.log") && jq -e 'type == "array" and length == 2' "$window" >/dev/null; then
                jq -c --arg repository "$name" --arg commit "$commit" --arg parent "$parent" --arg date "$date" \
                    '{repository: $repository, commit: $commit, parent: $parent, date: $date, status: "ok", head: .[1], base: .[0], erosion_delta: (.[1].erosion_ratio - .[0].erosion_ratio)}' \
                    "$window" >> "$repo_output/commits.jsonl"
            else
                jq -nc --arg repository "$name" --arg commit "$commit" --arg parent "$parent" --arg date "$date" \
                    '{repository: $repository, commit: $commit, parent: $parent, date: $date, status: "analysis-error"}' >> "$repo_output/commits.jsonl"
            fi
        done
    rm -f "$repo_output/.window.json"
done
failed=$(find "$output" -type f -name status -exec grep -l '^1$' {} + | wc -l | tr -d ' ')
[ "$failed" -eq 0 ]
