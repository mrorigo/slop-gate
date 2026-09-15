#!/bin/sh
set -eu

usage() { printf '%s\n' 'usage: calibration/monthly.sh --binary PATH --only LIST --output PATH'; }
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
tmp=$(mktemp -d "${TMPDIR:-/tmp}/slop-gate-monthly.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM

awk '/^name = / { name=$3; gsub(/"/, "", name) } /^url = / { url=$3; gsub(/"/, "", url); print name "\t" url }' "$root/calibration/manifest.toml" |
while IFS='	' read -r name url; do
    case ",$only," in *",$name,"*) ;; *) continue ;; esac
    checkout="$tmp/$name"; dir="$output/$name"; mkdir -p "$dir"
    if ! git clone --quiet "$url" "$checkout" 2>"$dir/stderr.log"; then printf 'clone failed\n' >> "$dir/stderr.log"; continue; fi
    branch=$(git -C "$checkout" symbolic-ref --short HEAD)
    : > "$dir/monthly.jsonl"
    month=1
    while [ "$month" -le 12 ]; do
        boundary=$(date -v-"$((12 - month))"m -v1d '+%Y-%m-%d' 2>/dev/null || date -d "$(date +%Y)-$((month + 1))-01" '+%Y-%m-%d')
        commit=$(git -C "$checkout" rev-list --first-parent --before="$boundary" -n 1 "$branch" || true)
        if [ -z "$commit" ]; then
            jq -nc --arg month "$boundary" '{month:$month,status:"missing"}' >> "$dir/monthly.jsonl"
        else
            window="$dir/.month.json"
            if (cd "$checkout" && "$binary" history --ref "$commit" --count 1 --complexity-cutoffs 5,10,15 --format json > "$window" 2>>"$dir/stderr.log") && jq -e 'length == 1' "$window" >/dev/null; then
                jq -c --arg month "$boundary" --arg commit "$commit" '{month:$month,commit:$commit,status:"ok",summary:.[0]}' "$window" >> "$dir/monthly.jsonl"
            else
                jq -nc --arg month "$boundary" --arg commit "$commit" '{month:$month,commit:$commit,status:"analysis-error"}' >> "$dir/monthly.jsonl"
            fi
            rm -f "$window"
        fi
        month=$((month + 1))
    done
done
