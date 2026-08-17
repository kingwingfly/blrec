# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & run

```bash
cargo build --release     # binary at target/release/blrec
cargo run -- <args>       # run with arguments
```

**System requirement:** FFmpeg 9 shared libraries (`libavcodec`, `libavformat`, `libavutil`, `libswresample`). The `ffmpeg-next` crate links against these at build time.

There are no tests, no lint configuration, and no CI in this repository.

## Architecture

A CLI tool for recording audio/video from Bilibili live streams. Three subcommands: `auth`, `pipe`, `record`.

### Module map

| Module | Purpose |
|---|---|
| `main.rs` | Sets up `tracing-subscriber` (output to stderr, `RUST_LOG` env filter), calls `command::run()` |
| `command.rs` | `clap` derive-macro CLI definition (`#[derive(Parser)]`, `#[derive(Subcommand)]`) and subcommand dispatch |
| `auth.rs` | QR login, cookie login, logout, credential check |
| `cookies.rs` | Credential persistence — read/write `.blrec/auth.json`, interact with `api_req`'s global `COOKIE_JAR` |
| `api.rs` | Three `#[derive(ApiCaller)]` structs: `AuthApi` (passport.bilibili.com), `BiliApi` (api.bilibili.com), `LiveApi` (api.live.bilibili.com) |
| `payload.rs` | Request types with `#[derive(Payload)]` — path, method, and serialization mode declared via `#[api_req(...)]` |
| `response.rs` | Deserialize-only response types mirroring Bilibili's JSON shape |
| `live.rs` | Room ID resolution, live-status polling (every 5 s), stream URL fetching with modern→legacy fallback |
| `pipe.rs` | Stream raw FLV to stdout with reconnection and `FlvStripper`; optional TCP listen mode for multi-client serving |
| `record.rs` | Record to file — `AudioRecorder` (ffmpeg-next: AAC→WAV/MP3/FLAC) or direct FLV download via `download_connection()` |
| `flv.rs` | `FlvStripper` — strips redundant FLV headers on reconnect so downstream ffmpeg sees one continuous stream. `FlvFilter` — wraps `FlvStripper` and optionally drops audio (0x08) or video (0x09) tags at the byte level |
| `error.rs` | `LiveError` enum (thiserror) — currently unused in application code; the codebase uses `anyhow::Result` throughout |

### Data flow

```
CLI args → resolve_room_id → wait_for_live (poll / 5 s)
         → get_stream_url (modern API, fallback to legacy)
         → [pipe] reqwest streaming → FlvStripper → stdout
         → [record audio] ffmpeg-next: demux FLV → decode AAC → resample → encode (wav/mp3/flac)
         → [record video] reqwest streaming → FlvFilter → write FLV file
```

### Key patterns

- **API calls** use the `api_req` crate: `#[derive(ApiCaller)]` on structs for the HTTP client, `#[derive(Payload)]` on request types. Cookies flow through a global `COOKIE_JAR` managed by `api_req`.
- **Stream URLs** are fetched from the modern `getRoomPlayInfo` API first, with silent fallback to the legacy `playUrl` API. The CDN requires `Referer` and `User-Agent` headers.
- **Reconnection** uses exponential backoff (1 s → 30 s max, `1 << attempt` capped at 5) with a hard cap of 20 reconnects. Between reconnects/EOF, the code polls `get_live_status` to detect stream end.
- **Cancellation** in `record` uses `tokio_util::sync::CancellationToken`, fired by Ctrl+C signal handler or timeout.
- **FFmpeg-native encoding** (`AudioRecorder`) runs on a blocking threadpool via `tokio::task::spawn_blocking` because `ffmpeg-next` is synchronous. Decoder and encoder persist across reconnections — only the input stream is reopened.
- **FLV byte manipulation** (`flv.rs`) works at the tag level without a full demuxer: it parses the 11-byte tag header, reads the data size, and skips or copies whole tags. The `FlvStripper` removes the FLV header + leading script-data tags on reconnect so the concatenated byte stream looks like one continuous FLV.
- **FLV recording** uses `download_connection()` to stream directly to a `.flv` file with `FlvFilter` for optional track stripping. No external process is ever spawned.

### Important details

- All diagnostics go to **stderr** via `tracing`. Only the final "Recording saved to …" message is printed to **stdout**. The `pipe` subcommand writes only FLV bytes to stdout — downstream tools see a clean stream.
- Credentials are stored in `.blrec/auth.json` with permissions `600` (dir `700`). This file is git-ignored.
- The project uses Rust **edition 2024**.
- The `--quality` flag takes Bilibili's quality number (`qn`): 150 = 高清 (default), 250 = 超清, 400 = 蓝光, 10000 = 原画.
- **Never spawn external processes.** No `std::process::Command`, no `tokio::process::Command`. All processing (FLV manipulation, audio encoding) is done in-process via `ffmpeg-next`.
