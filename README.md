# muz

Rust CLI that continuously shuffles a YouTube playlist and plays **audio-only** for each item.

## Prerequisites

Install these command-line tools first:

- `yt-dlp` (playlist parsing and track metadata)
- `mpv` (audio playback with proper pause support)

On macOS (Homebrew):

```bash
brew install yt-dlp mpv
```

## Build

```bash
cargo build --release
```

## Run

```bash
cargo run -- "https://www.youtube.com/playlist?list=YOUR_PLAYLIST_ID"
```

Optional flags:

- `--retry-delay-secs <N>`: wait time before re-fetching playlist after an error (default: `5`).

## Behavior

- Fetches playlist entries with `yt-dlp`.
- Randomizes order each cycle.
- Plays audio-only streams through `mpv` (with `yt-dlp` backend for stream resolution).
- While a track is playing:
  - `n` — skip to the next track.
  - `p` — toggle pause/resume (uses mpv IPC for glitch-free pause).
  - `q` — exit cleanly.
- Playback shows an in-place status line, e.g. `[playing] 01:23 / 03:45`.
- Repeats forever until you stop it (`Ctrl+C`).

## Android (Termux)

This repository includes a Termux-friendly script at [scripts/muz-android-screenoff.sh](scripts/muz-android-screenoff.sh) for Android devices.

Install prerequisites in Termux:

```bash
pkg update && pkg upgrade
pkg install yt-dlp mpv jq socat coreutils termux-api
```

Run:

```bash
./scripts/muz-android-screenoff.sh "https://www.youtube.com/playlist?list=YOUR_PLAYLIST_ID"
```

Controls while playing:

- `n` — skip
- `p` — pause/resume
- `q` — quit

## License

Licensed under either of [LICENSE-MIT](LICENSE-MIT) or [LICENSE-APACHE](LICENSE-APACHE) at your option.

Third-party dependency license notices are in [THIRD_PARTY_LICENSES.html](THIRD_PARTY_LICENSES.html).
