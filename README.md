# blarec — Bilibili Live Audio Recorder

Record audio from [Bilibili](https://live.bilibili.com) live streams.

## Installation

**Requirements:** FFmpeg 8.x shared libraries (`libavcodec`, `libavformat`, `libavutil`,
`libswresample`).

| OS | Install |
|---|---|
| Arch | `pacman -S ffmpeg` |
| Debian / Ubuntu | `apt install libavcodec-dev libavformat-dev libavutil-dev libswresample-dev` |
| macOS | `brew install ffmpeg@8` |

Build:

```bash
cargo build --release
```

The binary is at `target/release/blarec`.

## Authentication

Live stream URLs are public, but logging in gives access to API features.
Credentials are stored at `.blarec/auth.json`.

### QR code login

```bash
blarec auth login
```

Scan the QR code displayed in the terminal with the Bilibili mobile app.

### Login with browser cookies

```bash
blarec auth use-cookies "SESSDATA=xxx; bili_jct=yyy; DedeUserID=zzz"
```

Copy the full cookie header from your browser's developer tools.
At minimum `SESSDATA` is required; `bili_jct` is also needed for logout.

### Check & logout

```bash
blarec auth check     # verify stored cookies are valid
blarec auth logout    # clear stored credentials
```

## Usage

### `listen` — pipe raw audio to ffmpeg

Stream raw FLV bytes to stdout so an external `ffmpeg` process can encode them:

```bash
# Record to FLAC
blarec listen 12345 | ffmpeg -f flv -i - -c:a flac "$(date +%Y%m%d_%H%M%S).flac"

# Record to MP3 (320 kbps)
blarec listen 12345 | ffmpeg -f flv -i - -vn -c:a libmp3lame -b:a 320k output.mp3

# Extract raw AAC (no re-encode)
blarec listen 12345 | ffmpeg -f flv -i - -vn -c:a copy output.aac
```

> **Important:** Always use `-f flv -i -` — Bilibili delivers audio in AAC inside
> an HTTP-FLV container. The stream URL is fetched automatically.

### `record` — direct to file (internal ffmpeg)

Encodes with the system FFmpeg libraries — no external `ffmpeg` binary needed.

```bash
# WAV  (PCM 16-bit, 48 kHz stereo)
blarec record 12345 -f wav

# MP3  (192 kbps)
blarec record 12345 -f mp3

# FLAC (lossless)
blarec record 12345 -f flac

# Custom output path and auto-stop
blarec record 12345 -f mp3 -o my_recording.mp3 --timeout 60
```

Output files are auto-named `{room_id}_{YYYYmmdd_HHMMSS}.{ext}` unless `-o` is
given.

### Options

| Option | Description |
|---|---|
| `-q, --quality <qn>` | Stream quality (default: 150 = 高清). 250 = 超清, 400 = 蓝光, 10000 = 原画 |
| `--timeout <secs>` | Stop automatically after N seconds |

## Stream events handled

| Event | Behaviour |
|---|---|
| Stream offline | Poll every 5 s until live (or timeout) |
| Stream goes live | Start / resume recording |
| Stream ends | Clean exit, finalise output file |
| Network drop / URL rotation | Reconnect with exponential backoff (1 s → 30 s max) |
| Pipe broken (`listen`) | Clean exit (downstream ffmpeg exited) |
| Ctrl+C (`record`) | Flush buffered audio, write trailer, exit |

## How it works

1. **Room resolution** — short ID → real room ID via `room/v1/Room/room_init`
2. **Live detection** — polls `room/v1/Room/get_info` every 5 s for `live_status`
3. **Stream URL** — `xlive/web-room/v2/index/getRoomPlayInfo` (modern) with
   fallback to `room/v1/Room/playUrl` (legacy)
4. **Audio** — AAC inside HTTP-FLV, fetched with `reqwest` (`listen`) or
   demuxed directly by FFmpeg (`record`)
5. **Encoding** — `ffmpeg-next` crate links system FFmpeg libraries for
   AAC → PCM → WAV / MP3 / FLAC

## Example: scheduled recording

```bash
# Record a 30-minute clip starting at 20:00
blarec record 12345 -f flac --timeout 1800
```

## License

MIT
