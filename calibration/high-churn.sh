#!/bin/sh
set -eu

usage() { printf '%s\n' 'usage: calibration/high-churn.sh --binary PATH --only LIST --output PATH'; }
binary=''; only=''; output=''
while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary) binary=$2; shift 2 ;;
        --only) only=$2; shift 2 ;;
        --output) output=$2; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done
[ -n "$binary" ] && [ -n "$only" ] && [ -n "$output" ] || { usage >&2; exit 2; }
command -v git >/dev/null || exit 2
command -v jq >/dev/null || exit 2
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=$(CDPATH= cd -- "$(dirname -- "$binary")" && pwd)/$(basename -- "$binary")
mkdir -p "$output"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/slop-gate-churn.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM

awk '/^name = / { name=$3; gsub(/"/, "", name) } /^url = / { url=$3; gsub(/"/, "", url); print name "\t" url }' "$root/calibration/manifest.toml" |
while IFS='	' read -r name url; do
    case ",$only," in *",$name,"*) ;; *) continue ;; esac
    checkout="$tmp/$name"; dir="$output/$name"; mkdir -p "$dir"
    if ! git clone --quiet "$url" "$checkout" 2>"$dir/stderr.log"; then printf 'clone failed\n' >> "$dir/stderr.log"; continue; fi
    branch=$(git -C "$checkout" symbolic-ref --short HEAD)
    metadata="$dir/.metadata.json"
    git -C "$checkout" log --first-parent --format='%H%x09%P%x09%cI' -n 200 |
        while IFS='	' read -r commit parent date; do
            lines=$(git -C "$checkout" diff --numstat "$parent" "$commit" | awk -F '	' '{ if ($1 ~ /^[0-9]+$/ && $2 ~ /^[0-9]+$/) total += $1 + $2 } END { print total + 0 }')
            printf '%s\t%s\t%s\t%s\n' "$commit" "$parent" "$date" "$lines"
        done |
        sort -t '	' -k4,4nr | head -5 |
        awk -F '	' '{ printf "{\"commit\":\"%s\",\"parent\":\"%s\",\"date\":\"%s\",\"changed_lines\":%s}\n", $1, $2, $3, $4 }' |
        jq -s . > "$metadata"
    : > "$dir/high-churn.jsonl"
    jq -r '.[] | [.commit, .parent, .date, .changed_lines] | @tsv' "$metadata" |
    while IFS='	' read -r commit parent date changed_lines; do
        window="$dir/.window.json"
        if (cd "$checkout" && "$binary" history --ref "$commit" --count 2 --complexity-cutoffs 5,10,15 --format json > "$window" 2>>"$dir/stderr.log") && jq -e 'length == 2' "$window" >/dev/null; then
            jq -c --arg repository "$name" --arg commit "$commit" --arg parent "$parent" --arg date "$date" --argjson changed_lines "$changed_lines" '{repository:$repository,commit:$commit,parent:$parent,date:$date,changed_lines:$changed_lines,status:"ok",base:.[0],head:.[1],erosion_delta:(.[1].erosion_ratio - .[0].erosion_ratio)}' "$window" >> "$dir/high-churn.jsonl"
        else
            jq -nc --arg repository "$name" --arg commit "$commit" --arg parent "$parent" --arg date "$date" --argjson changed_lines "$changed_lines" '{repository:$repository,commit:$commit,parent:$parent,date:$date,changed_lines:$changed_lines,status:"analysis-error"}' >> "$dir/high-churn.jsonl"
        fi
    done
    rm -f "$metadata" "$dir/.window.json"
done
