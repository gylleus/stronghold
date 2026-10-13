#!/bin/sh
# Stronghold SessionStart hook: records which Claude session runs in which Orca
# pane by appending one JSON line to session-map.jsonl in the Stronghold data dir.
# Always exits 0 and never writes to stdout (SessionStart stdout is fed to Claude).

exec >/dev/null 2>&1

payload=$(cat | tr '\n\r' '  ')
[ -n "${ORCA_PANE_KEY:-}" ] || exit 0

# Raw (still JSON-escaped) contents of a top-level string field in the payload.
field() {
  printf '%s' "$payload" | sed -n -E 's/.*"'"$1"'"[[:space:]]*:[[:space:]]*"(([^"\\]|\\.)*)".*/\1/p'
}
esc() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g; s/[[:cntrl:]]/ /g'; }

session_id=$(field session_id)
[ -n "$session_id" ] || exit 0

# MYRSTACK_DATA_DIR is the variable's name from before the app was renamed.
dir=${STRONGHOLD_DATA_DIR:-${MYRSTACK_DATA_DIR:-$HOME/Library/Application Support/Stronghold}}
file="$dir/session-map.jsonl"
mkdir -p "$dir" || exit 0

now=$(date +%s) || now=0
printf '{"paneKey":"%s","tabId":"%s","worktreeId":"%s","sessionId":"%s","transcriptPath":"%s","cwd":"%s","source":"%s","at":%s}\n' \
  "$(esc "$ORCA_PANE_KEY")" "$(esc "${ORCA_TAB_ID:-}")" "$(esc "${ORCA_WORKTREE_ID:-}")" \
  "$session_id" "$(field transcript_path)" "$(field cwd)" "$(field source)" "$((now * 1000))" \
  >>"$file" || exit 0

size=$(wc -c <"$file") || exit 0
if [ "$size" -gt 1048576 ]; then
  tmp="$file.tmp.$$"
  tail -n 2000 "$file" >"$tmp" && mv -f "$tmp" "$file"
  rm -f "$tmp"
fi
exit 0
