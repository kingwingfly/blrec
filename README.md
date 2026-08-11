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

## Post-processing: transcode to AV1

`blrec` never spawns external processes — recorded `.flv` files keep the
original H.264 video untouched.  To shrink them afterwards, transcode to AV1
with the `ffmpeg` CLI binary (same package as the shared libraries on most
distros).  Check that your build has the encoder first:

```bash
ffmpeg -hide-banner -encoders | grep svtav1
```

```bash
mkdir -p chuan
VIDEO=12345_20260811_203000.flv

ffmpeg -i "$VIDEO" \
  -c:v libsvtav1 -crf 32 -preset 6 \
  -svtav1-params keyint=3s:enable-variance-boost=1 \
  -pix_fmt yuv420p10le \
  -c:a copy \
  -movflags +faststart \
  "chuan/${VIDEO%.flv}.mp4"
```

The output must be `.mp4` (or `.mkv`) — FLV cannot carry an AV1 track.

| Flag | Why |
|---|---|
| `-c:v libsvtav1` | SVT-AV1 encoder — fast enough for long stream VODs |
| `-crf 32 -preset 6` | Quality/speed balance; lower CRF = better quality, lower preset = slower but smaller |
| `keyint=3s` | Keyframe every 3 seconds, framerate-independent — keeps output seekable |
| `enable-variance-boost=1` | Allocates more bits to flat/dark areas — helps with typical stream backgrounds |
| `-pix_fmt yuv420p10le` | 10-bit internally reduces banding even from 8-bit sources |
| `-c:a copy` | Stream-copies the AAC audio — no generational loss, no extra time |
| `-movflags +faststart` | Moves the MP4 index to the front for instant playback/streaming |

Batch a directory of recordings (`fish`):

```fish
mkdir -p chuan
for f in *.flv
    ffmpeg -i "$f" -c:v libsvtav1 -crf 32 -preset 6 \
      -svtav1-params keyint=3s:enable-variance-boost=1 \
      -pix_fmt yuv420p10le -c:a copy -movflags +faststart \
      "chuan/$(string replace .flv .mp4 $f)"
end
```

In `bash`: `for f in *.flv; do ffmpeg -i "$f" … "chuan/${f%.flv}.mp4"; done`.

Record first, transcode later — SVT-AV1 at these settings is generally not
realtime.

## Stream lifecycle & behaviour

### What happens when…

**…the stream hasn't started yet?**

Both `pipe` and `record` call `wait_for_live()`, which polls the Bilibili API
every 5 seconds until `live_status == 1`.  No output file is created, no bytes
are written — the process simply waits (potentially forever, unless `--timeout`
is set).

```
 INFO Stream offline, waiting...       (repeats every 5 s)
 INFO Stream is now live!
```

**…the stream is live and recording?**

| Format | What happens |
|---|---|
| FLV | Raw FLV bytes are streamed from the CDN and appended to the output file.  `FlvFilter` drops audio / video tags if `--no-audio` / `--no-video` is set. |
| WAV / MP3 / FLAC | ffmpeg-next opens the stream URL, demuxes FLV, decodes AAC, resamples (48 kHz stereo), encodes to the target format, and writes via the muxer.  Encoder, resampler, and muxer **persist** across reconnections — only the input stream is reopened. |

**…the streamer stops broadcasting?**

| Path | Detection | Behaviour |
|---|---|---|
| `pipe` | Active: polls `get_live_status` every 5 s during streaming | Exits cleanly as soon as `live_status == 0` |
| `record --format flv` | Passive: waits for the CDN to close the connection (EOF or error) | On EOF, checks `still_live()`.  If offline → clean exit.  If still live → reconnect. |
| `record --format wav/mp3/flac` | ffmpeg `input_with_dictionary` hits EOF | Checks `still_live()`.  If offline → exits loop, calls `write_trailer()`, file is valid.  If still live → reconnects. |

No zeroes or silence are ever written — when the stream ends the recording stops
and the output file is finalised.

> **Important:** `blrec record` makes **one recording per invocation**.  When the
> stream ends the process exits.  It does **not** go back to waiting for the
> streamer to start again.  To record a subsequent stream, run `blrec record`
> again — it will create a new file (the auto-generated name includes a fresh
> timestamp).

**…the network drops or the CDN rotates the stream URL?**

Reconnection with exponential backoff: **1 s → 2 s → 4 s → 8 s → 16 s → 30 s**
(max), up to **20 attempts**.  On reconnect the `FlvStripper` / `FlvFilter`
strips the duplicate FLV header and script-data tags so the concatenated byte
stream stays valid.  For audio, ffmpeg is given the new URL and picks up where
it left off.

If all 20 reconnects are exhausted the process exits with an error.  Any data
already written to the output file is kept.

**…Ctrl+C is pressed?**

| Format | Behaviour |
|---|---|
| FLV | `CancellationToken` is set; the download loop breaks immediately.  The `.flv` file is a valid truncated raw byte stream — no trailer is needed. |
| WAV / MP3 / FLAC | The encoder drains buffered frames, the muxer writes the trailer, and the file is closed.  The result is a valid, playable truncated recording. |

**…`--timeout` expires?**

Same as Ctrl+C — the timeout spawn sets the same `CancellationToken`, so cleanup
is identical.

### Summary

| Event | `pipe` | `record --format flv` | `record --format wav/mp3/flac` |
|---|---|---|---|
| Stream offline (before start) | Wait forever (poll / 5 s) | Wait forever (poll / 5 s) | Wait forever (poll / 5 s) |
| Stream goes live | Start piping to stdout | Start appending to `.flv` | Start encoding to file |
| Streamer stops | Detect via API (5 s poll), exit | Wait for CDN EOF, then exit | ffmpeg EOF, then exit |
| Network drop | Reconnect (backoff, max 20) | Reconnect (backoff, max 20) | Reconnect (backoff, max 20) |
| Pipe broken / downstream closes | Exit cleanly | N/A | N/A |
| Ctrl+C | Stop immediately | Stop immediately (valid file) | Drain + write trailer (valid file) |
| Timeout | Stop | Stop (valid file) | Drain + write trailer (valid file) |

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
