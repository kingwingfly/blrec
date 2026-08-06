# blrec — Bilibili Live Recorder

Record audio from [Bilibili](https://live.bilibili.com) live streams, or pipe the
raw FLV stream to external tools like `ffmpeg` / `ffplay`.

## Installation

**Requirements:** FFmpeg 8.x shared libraries (`libavcodec`, `libavformat`,
`libavutil`, `libswresample`).

| OS | Install |
|---|---|
| Arch | `pacman -S ffmpeg` |
| Debian / Ubuntu | `apt install libavcodec-dev libavformat-dev libavutil-dev libswresample-dev` |

```bash
cargo build --release
# binary at target/release/blrec
```

## Authentication

Live stream URLs are public, but logging in gives access to API features.
Credentials are stored at `.blarec/auth.json`.

### QR code login

```bash
blrec auth login
```

Scan the QR code displayed in the terminal with the Bilibili mobile app.

### Cookie login

```bash
blrec auth use-cookies "SESSDATA=xxx; bili_jct=yyy; DedeUserID=zzz"
```

Copy the full cookie header from your browser's developer tools (at minimum
`SESSDATA`; `bili_jct` is also needed for logout).

```bash
blrec auth check     # verify stored cookies are valid
blrec auth logout    # clear stored credentials
```

## Usage

### `pipe` — raw FLV to stdout

Pipe the full HTTP-FLV stream (audio + video) to stdout so external tools can
process it. All status messages go to **stderr**.

```bash
# Watch live stream
blrec pipe 12345 | ffplay -f flv -

# Record audio only (FLAC)
blrec pipe 12345 | ffmpeg -f flv -i - -vn -c:a flac "$(date +%Y%m%d_%H%M%S).flac"

# Record audio only (MP3, 320 kbps)
blrec pipe 12345 | ffmpeg -f flv -i - -vn -c:a libmp3lame -b:a 320k output.mp3

# Record audio only (raw AAC, no re-encode)
blrec pipe 12345 | ffmpeg -f flv -i - -vn -c:a copy output.aac

# Record video + audio (remux to MP4)
blrec pipe 12345 | ffmpeg -f flv -i - -c copy output.mp4

# Record video + audio (remux to MKV)
blrec pipe 12345 | ffmpeg -f flv -i - -c copy output.mkv

# Record video + re-encode audio to AAC
blrec pipe 12345 | ffmpeg -f flv -i - -c:v copy -c:a aac -b:a 192k output.mp4

# Record video only (no audio)
blrec pipe 12345 | ffmpeg -f flv -i - -an -c:v copy output.mp4

# Serve live stream to multiple clients via TCP
blrec pipe 12345 -l 127.0.0.1:3000
# Then connect:  ffplay -f flv tcp://127.0.0.1:3000
```

> **Important:** Use `-f flv -i -` — Bilibili streams are HTTP-FLV with AAC audio
> and H.264 video.

### `record` — record to file

Encodes audio or saves video with the system FFmpeg libraries — no external
`ffmpeg` binary needed. Format may be given explicitly (`-f`) or inferred from
the output file extension.

```bash
# ── Audio-only ──────────────────────────────────────────

# WAV  (PCM 16-bit, 48 kHz stereo)
blrec record 12345 -f wav

# MP3  (192 kbps)
blrec record 12345 -f mp3

# FLAC (lossless)
blrec record 12345 -f flac

# ── Video ──────────────────────────────────────────────

# FLV  (raw stream copy, both audio + video)
blrec record 12345 -f flv

# Video only (no audio)
blrec record 12345 -f flv --no-audio

# Audio only (AAC in FLV container)
blrec record 12345 -f flv --no-video

# ── Convenience ─────────────────────────────────────────

# Format inferred from output extension
blrec record 12345 -o my_stream.flv
blrec record 12345 -o my_stream.flv

# Auto-stop after 60 seconds
blrec record 12345 -f mp3 --timeout 60
```

Output files are auto-named `{room_id}_{YYYYmmdd_HHMMSS}.{ext}` unless `-o` is
given.  `-f` and `-o` extension must agree when both are specified.

### Options

| Option | Description |
|---|---|
| `-q, --quality <qn>` | Stream quality (default: 150 = 高清). 250 = 超清, 400 = 蓝光, 10000 = 原画 |
| `--timeout <secs>` | Stop automatically after N seconds |

## Stream events handled

| Event | Behaviour |
|---|---|
| Stream offline | Poll every 5 s until live (or timeout) |
| Stream goes live | Start / resume |
| Stream ends | Clean exit, finalise output file |
| Network drop / URL rotation | Reconnect with exponential backoff (1 s → 30 s max) |
| Pipe broken (`pipe`) | Clean exit (downstream process closed stdin) |
| Ctrl+C (`record`) | Flush buffered audio, write trailer, exit |

## How it works

1. **Room resolution** — short ID → real room ID via `room/v1/Room/room_init`
2. **Live detection** — polls `room/v1/Room/get_info` every 5 s for `live_status`
3. **Stream URL** — `xlive/web-room/v2/index/getRoomPlayInfo` (modern) with
   fallback to `room/v1/Room/playUrl` (legacy). CDN requires `Referer` header.
4. **`pipe`** — `reqwest` streams FLV bytes directly to stdout
5. **`record`** — FFmpeg demuxes FLV, decodes AAC, resamples, encodes to
   WAV / MP3 / FLAC. Encoder + muxer persist across reconnections.

## License

MIT
