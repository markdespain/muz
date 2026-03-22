#!/data/data/com.termux/files/usr/bin/bash
set -euo pipefail

PLAYLIST_URL="${1:-}"
if [[ -z "$PLAYLIST_URL" ]]; then
  echo "Usage: ./scripts/muz-android-screenoff.sh <youtube-playlist-url>"
  exit 1
fi

SOCK="/tmp/muz-mpv.sock"
MPV_PID=""
PAUSED=0
NOTIF_ID=7771

fmt_time() {
  local s="${1:-0}"
  printf "%02d:%02d" $((s/60)) $((s%60))
}

notify() {
  local title="$1"
  local content="$2"
  termux-notification --id "$NOTIF_ID" --ongoing --title "$title" --content "$content" >/dev/null 2>&1 || true
}

cleanup() {
  [[ -n "$MPV_PID" ]] && kill "$MPV_PID" 2>/dev/null || true
  rm -f "$SOCK"
  stty echo icanon 2>/dev/null || true
  termux-notification-remove "$NOTIF_ID" >/dev/null 2>&1 || true
  termux-wake-unlock >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

termux-wake-lock >/dev/null 2>&1 || true
echo "Controls: n=next, p=pause/resume, q=quit"
notify "muz" "Starting…"

while true; do
  mapfile -t ITEMS < <(
    yt-dlp --flat-playlist --dump-single-json "$PLAYLIST_URL" 2>/dev/null \
      | jq -r '.entries[]? | [.id // "", .title // "(untitled)"] | @tsv' \
      | shuf
  )

  if [[ ${#ITEMS[@]} -eq 0 ]]; then
    notify "muz" "No playlist items found. Retrying in 10s."
    sleep 10
    continue
  fi

  for row in "${ITEMS[@]}"; do
    id="${row%%$'\t'*}"
    title="${row#*$'\t'}"
    url="https://www.youtube.com/watch?v=$id"

    total="$(yt-dlp --no-playlist --print '%(duration)s' "$url" 2>/dev/null | head -n1 || echo 0)"
    [[ "$total" =~ ^[0-9]+$ ]] || total=0

    rm -f "$SOCK"
    mpv --no-video --ytdl --really-quiet --input-ipc-server="$SOCK" "$url" >/dev/null 2>&1 &
    MPV_PID=$!

    for _ in $(seq 1 100); do
      [[ -S "$SOCK" ]] && break
      sleep 0.05
    done

    start_ts=$(date +%s)
    paused_total=0
    paused_since=0
    PAUSED=0

    stty -echo -icanon time 0 min 0
    notify "muz" "Playing: $title"

    while kill -0 "$MPV_PID" 2>/dev/null; do
      now=$(date +%s)
      if [[ $PAUSED -eq 1 ]]; then
        elapsed=$((paused_since - start_ts - paused_total))
        state="paused"
      else
        elapsed=$((now - start_ts - paused_total))
        state="playing"
      fi
      (( elapsed < 0 )) && elapsed=0

      if (( total > 0 )); then
        status="[$state] $(fmt_time "$elapsed") / $(fmt_time "$total")"
      else
        status="[$state] $(fmt_time "$elapsed") / --:--"
      fi

      printf "\rStatus: %s\033[K" "$status"
      notify "muz" "$title — $status"

      key="$(dd bs=1 count=1 2>/dev/null || true)"
      case "$key" in
        n|N)
          kill "$MPV_PID" 2>/dev/null || true
          wait "$MPV_PID" 2>/dev/null || true
          MPV_PID=""
          printf "\r\033[KSkipped.\n"
          notify "muz" "Skipped: $title"
          break
          ;;
        p|P)
          printf '{ "command": ["cycle", "pause"] }\n' \
            | socat - UNIX-CONNECT:"$SOCK" >/dev/null 2>&1 || true
          if [[ $PAUSED -eq 0 ]]; then
            PAUSED=1
            paused_since=$(date +%s)
          else
            PAUSED=0
            resumed=$(date +%s)
            paused_total=$((paused_total + resumed - paused_since))
            paused_since=0
          fi
          ;;
        q|Q)
          kill "$MPV_PID" 2>/dev/null || true
          wait "$MPV_PID" 2>/dev/null || true
          MPV_PID=""
          printf "\r\033[KQuit.\n"
          notify "muz" "Stopped."
          exit 0
          ;;
      esac

      sleep 1.0
    done

    [[ -n "$MPV_PID" ]] && wait "$MPV_PID" 2>/dev/null || true
    MPV_PID=""
    rm -f "$SOCK"
    stty echo icanon 2>/dev/null || true
    printf "\r\033[K"
  done
done
