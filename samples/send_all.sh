#!/usr/bin/env bash
# Uploads every sample file to the service and prints status, chunk count and time per file.
# Usage: ./send_all.sh [base_url] [dir]   (defaults: http://localhost:3000, this script's folder)
# Responses are saved to <dir>/out/<file>.json for inspection. Exits 1 if any upload failed.
set -uo pipefail

url="${1:-http://localhost:3000}/documents"
dir="${2:-$(cd "$(dirname "$0")" && pwd)}"
out="$dir/out"
command -v jq >/dev/null || { echo "jq is required (brew install jq)" >&2; exit 2; }
mkdir -p "$out"

failed=0
total=0
printf '%-20s %6s %7s %8s  %s\n' FILE STATUS CHUNKS TIME ERROR
for f in "$dir"/*; do
  [[ -f "$f" && "$f" != *.sh ]] || continue
  name=$(basename "$f")
  total=$((total + 1))
  meta=$(curl -s -o "$out/$name.json" -w '%{http_code} %{time_total}' -F "file=@$f" "$url") || meta="000 0"
  read -r status secs <<<"$meta"
  chunks=$(jq -r '.chunks | length // "-"' "$out/$name.json" 2>/dev/null || echo -)
  error=$(jq -r '.error // ""' "$out/$name.json" 2>/dev/null || echo "no JSON response")
  [[ "$status" == 200 ]] || failed=$((failed + 1))
  printf '%-20s %6s %7s %7.2fs  %s\n' "$name" "$status" "$chunks" "$secs" "$error"
done

echo "$((total - failed))/$total succeeded; responses in $out"
[[ $failed -eq 0 ]]
