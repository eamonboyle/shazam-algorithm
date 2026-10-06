#!/usr/bin/env bash
# Downloads the test library (60 CC BY 4.0 tracks by Kevin MacLeod, ~420 MB)
# listed in scripts/songs.txt into songs/library and songs/holdout.
# Files that already exist are skipped, so it is safe to re-run.
set -euo pipefail

BASE_URL="https://incompetech.com/music/royalty-free/mp3-royaltyfree"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/songs"

urlencode() {
  local s="$1" out="" c i
  for ((i = 0; i < ${#s}; i++)); do
    c="${s:i:1}"
    case "$c" in
      [a-zA-Z0-9.~_-]) out+="$c" ;;
      *) out+=$(printf '%%%02X' "'$c") ;;
    esac
  done
  printf '%s' "$out"
}

mkdir -p "$DEST/library" "$DEST/holdout"
fetched=0 skipped=0 failed=0
while IFS= read -r line; do
  [[ -z "$line" || "$line" == \#* ]] && continue
  folder="${line%%/*}"
  file="${line#*/}"
  target="$DEST/$folder/$file"
  if [[ -s "$target" ]]; then
    skipped=$((skipped + 1))
    continue
  fi
  echo "downloading $line"
  if curl -sSfL --retry 3 -o "$target.part" "$BASE_URL/$(urlencode "$file")"; then
    mv "$target.part" "$target"
    fetched=$((fetched + 1))
  else
    echo "  failed: $file" >&2
    rm -f -- "${target:?}.part"
    failed=$((failed + 1))
  fi
done < "$ROOT/scripts/songs.txt"

echo "done: $fetched downloaded, $skipped already present, $failed failed"
[[ $failed -eq 0 ]]
