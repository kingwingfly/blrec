use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::flv::FlvFilter;
use crate::live;

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Safari/605.1.15";
const REFERER: &str = "https://www.bilibili.com/";

// ── Format ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Format {
    Wav,
    Mp3,
    Flac,
    Mp4,
    Flv,
}

impl Format {
    pub fn is_audio_only(self) -> bool {
        matches!(self, Format::Wav | Format::Mp3 | Format::Flac)
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Wav => "wav",
            Format::Mp3 => "mp3",
            Format::Flac => "flac",
            Format::Mp4 => "mp4",
            Format::Flv => "flv",
        }
    }

    pub fn from_ext(ext: &str) -> Option<Self> {
        match ext {
            "wav" => Some(Format::Wav),
            "mp3" => Some(Format::Mp3),
            "flac" => Some(Format::Flac),
            "mp4" => Some(Format::Mp4),
            "flv" => Some(Format::Flv),
            _ => None,
        }
    }

    fn codec_name(self) -> &'static str {
        match self {
            Format::Wav => "pcm_s16le",
            Format::Mp3 => "libmp3lame",
            Format::Flac => "flac",
            Format::Mp4 | Format::Flv => unreachable!(),
        }
    }

    fn muxer_name(self) -> &'static str {
        match self {
            Format::Wav => "wav",
            Format::Mp3 => "mp3",
            Format::Flac => "flac",
            Format::Mp4 => "mp4",
            Format::Flv => "flv",
        }
    }
}

// ── Validation ──────────────────────────────────────────────────────

fn validate(format: Format, no_video: bool, no_audio: bool) -> Result<()> {
    if no_audio && no_video {
        anyhow::bail!("Cannot disable both audio and video — nothing to record");
    }
    if no_audio && format.is_audio_only() {
        anyhow::bail!(
            "Format '{}' is audio-only, incompatible with --no-audio",
            format.extension()
        );
    }
    Ok(())
}

// ── Helpers ─────────────────────────────────────────────────────────

fn output_path(real_room_id: i64, format: Format, output: Option<&str>) -> Result<PathBuf> {
    if let Some(o) = output {
        return Ok(PathBuf::from(o));
    }
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let filename = format!("{}_{}.{}", real_room_id, ts, format.extension());
    Ok(PathBuf::from(filename))
}

fn http_headers_dict() -> ffmpeg_next::Dictionary<'static> {
    let mut dict = ffmpeg_next::Dictionary::new();
    let headers = format!("Referer: {}\r\nUser-Agent: {}\r\n", REFERER, USER_AGENT);
    dict.set("headers", &headers);
    dict
}

enum ConnEnd {
    Eof,
    Cancelled,
}

// ── Audio encoder (wav / mp3 / flac) ────────────────────────────────

struct AudioRecorder {
    encoder: ffmpeg_next::codec::encoder::audio::Encoder,
    output: ffmpeg_next::format::context::Output,
}

impl AudioRecorder {
    fn new(path: &Path, format: Format) -> Result<Self> {
        ffmpeg_next::init().context("Failed to initialize ffmpeg")?;

        let codec = ffmpeg_next::codec::encoder::find_by_name(format.codec_name())
            .context(format!("Codec '{}' not found", format.codec_name()))?;

        let ctx = ffmpeg_next::codec::context::Context::new_with_codec(codec);
        let mut a_enc = ctx.encoder().audio()?;

        let rate = 48000;
        a_enc.set_rate(rate);
        a_enc.set_channel_layout(ffmpeg_next::ChannelLayout::STEREO);

        let sample_fmt = match format {
            Format::Wav => {
                ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Packed)
            }
            Format::Mp3 => {
                ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Planar)
            }
            Format::Flac => {
                ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Packed)
            }
            _ => unreachable!(),
        };
        a_enc.set_format(sample_fmt);

        if matches!(format, Format::Mp3) {
            a_enc.set_bit_rate(192_000);
        }

        let encoder = a_enc.open_as(codec)?;

        let mut output = ffmpeg_next::format::output_as(
            path.to_str().context("invalid output path")?,
            format.muxer_name(),
        )?;

        let mut out_stream = output.add_stream(codec)?;
        out_stream.set_parameters(&encoder);
        out_stream.set_time_base((1, rate));

        output.write_header()?;
        Ok(Self { encoder, output })
    }

    fn record_connection(
        &mut self,
        url: &str,
        cancel_token: &tokio_util::sync::CancellationToken,
    ) -> Result<ConnEnd> {
        let dict = http_headers_dict();
        let mut ictx = ffmpeg_next::format::input_with_dictionary(&url, dict)
            .context("Failed to open stream URL")?;

        let audio_stream = ictx
            .streams()
            .best(ffmpeg_next::media::Type::Audio)
            .context("No audio stream found")?;
        let audio_idx = audio_stream.index();

        let decoder_ctx =
            ffmpeg_next::codec::context::Context::from_parameters(audio_stream.parameters())?;
        let mut decoder = decoder_ctx.decoder().audio()?;

        let mut resampler = ffmpeg_next::software::resampling::Context::get(
            decoder.format(),
            decoder.channel_layout(),
            decoder.rate(),
            self.encoder.format(),
            self.encoder.channel_layout(),
            self.encoder.rate(),
        )?;

        for (stream, packet) in ictx.packets() {
            if cancel_token.is_cancelled() {
                return Ok(ConnEnd::Cancelled);
            }
            if stream.index() != audio_idx {
                continue;
            }
            decoder.send_packet(&packet)?;
            let mut decoded = ffmpeg_next::frame::Audio::empty();
            while decoder.receive_frame(&mut decoded).is_ok() {
                let mut resampled = ffmpeg_next::frame::Audio::empty();
                resampler.run(&decoded, &mut resampled)?;
                self.encoder.send_frame(&resampled)?;
                let mut encoded = ffmpeg_next::codec::packet::Packet::empty();
                while self.encoder.receive_packet(&mut encoded).is_ok() {
                    encoded.set_stream(0);
                    encoded.write_interleaved(&mut self.output)?;
                }
            }
        }

        decoder.send_eof()?;
        let mut decoded = ffmpeg_next::frame::Audio::empty();
        while decoder.receive_frame(&mut decoded).is_ok() {
            let mut resampled = ffmpeg_next::frame::Audio::empty();
            resampler.run(&decoded, &mut resampled)?;
            self.encoder.send_frame(&resampled)?;
            let mut encoded = ffmpeg_next::codec::packet::Packet::empty();
            while self.encoder.receive_packet(&mut encoded).is_ok() {
                encoded.set_stream(0);
                encoded.write_interleaved(&mut self.output)?;
            }
        }

        self.encoder.send_eof()?;
        let mut encoded = ffmpeg_next::codec::packet::Packet::empty();
        while self.encoder.receive_packet(&mut encoded).is_ok() {
            encoded.set_stream(0);
            encoded.write_interleaved(&mut self.output)?;
        }

        Ok(ConnEnd::Eof)
    }

    fn finish(&mut self) -> Result<()> {
        self.output.write_trailer()?;
        Ok(())
    }
}

// ── Download-based recording (FLV + MP4 temp) ───────────────────────

struct DownloadRecorder {
    path: PathBuf,
    filter: FlvFilter,
}

impl DownloadRecorder {
    fn new(path: &Path, keep_audio: bool, keep_video: bool) -> Self {
        Self {
            path: path.to_path_buf(),
            filter: FlvFilter::new(keep_audio, keep_video),
        }
    }

    async fn record_connection(
        &mut self,
        url: &str,
        cancel_token: &tokio_util::sync::CancellationToken,
    ) -> Result<ConnEnd> {
        let client = reqwest::Client::new();
        let resp = client
            .get(url)
            .header("Referer", REFERER)
            .header("User-Agent", USER_AGENT)
            .send()
            .await?;

        let mut stream = resp.bytes_stream();
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;

        loop {
            if cancel_token.is_cancelled() {
                return Ok(ConnEnd::Cancelled);
            }
            tokio::select! {
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            let output = self.filter.process(&bytes);
                            file.write_all(output).await?;
                        }
                        Some(Err(e)) => {
                            error!("Stream error: {e}");
                            break;
                        }
                        None => {
                            info!("Stream connection closed (EOF)");
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
        }

        Ok(ConnEnd::Eof)
    }

    fn mark_reconnect(&mut self) {
        self.filter.mark_reconnect();
    }
}

// ── MP4 remux (spawn ffmpeg CLI) ────────────────────────────────────

fn remux_flv_to_mp4(flv_path: &Path, mp4_path: &Path) -> Result<()> {
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-v", "error",
            "-err_detect", "ignore_err",
            "-fflags", "+discardcorrupt+genpts+igndts",
            "-i", &flv_path.to_string_lossy(),
            "-c", "copy",
            "-movflags", "+faststart",
            "-y",
            &mp4_path.to_string_lossy(),
        ])
        .output()
        .context("Failed to spawn ffmpeg for remux")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ffmpeg remux failed: {stderr}");
    }
    // Log ffmpeg stderr at debug level (warnings about truncated input are expected)
    if !output.stderr.is_empty() {
        tracing::debug!("ffmpeg: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    info!("Remuxed FLV -> MP4: {}", mp4_path.display());
    Ok(())
}

// ── Main record function ────────────────────────────────────────────

pub async fn record(
    room_id: i64,
    format: Format,
    output: Option<String>,
    quality: u32,
    timeout: Option<Duration>,
    no_video: bool,
    no_audio: bool,
) -> Result<()> {
    validate(format, no_video, no_audio)?;

    let real_id = live::resolve_room_id(room_id).await?;
    info!("Room {room_id} resolved to real ID {real_id}");

    let cancel_token = tokio_util::sync::CancellationToken::new();

    // Ctrl+C
    let ct = cancel_token.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Ctrl+C received, finishing...");
        ct.cancel();
    });

    // Timeout
    if let Some(t) = timeout {
        let ct = cancel_token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(t).await;
            info!("Timeout reached, finishing...");
            ct.cancel();
        });
    }

    record_inner(real_id, format, output, quality, no_video, no_audio, &cancel_token).await
}

async fn record_inner(
    real_id: i64,
    format: Format,
    output: Option<String>,
    quality: u32,
    no_video: bool,
    no_audio: bool,
    cancel_token: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    live::wait_for_live(real_id).await?;

    let dest = output_path(real_id, format, output.as_deref())?;

    let mut reconnect_count = 0u32;
    let max_reconnects = 20;

    if format.is_audio_only() {
        // ── Audio-only path (wav / mp3 / flac) ──
        let recorder = Arc::new(Mutex::new(AudioRecorder::new(&dest, format)?));

        loop {
            if cancel_token.is_cancelled() {
                info!("Cancelled.");
                break;
            }

            let url = fetch_url(real_id, quality).await?;
            info!("Connecting (reconnect #{reconnect_count})...");

            let rec = recorder.clone();
            let tok = cancel_token.clone();
            let result = tokio::task::spawn_blocking(move || {
                rec.blocking_lock().record_connection(&url, &tok)
            })
            .await??;

            match result {
                ConnEnd::Cancelled => break,
                ConnEnd::Eof => {
                    reconnect_count += 1;
                    if reconnect_count > max_reconnects {
                        error!("Too many reconnects ({max_reconnects})");
                        break;
                    }
                    if !still_live(real_id).await {
                        info!("Stream ended.");
                        break;
                    }
                    backoff(reconnect_count).await;
                }
            }
        }

        recorder.lock().await.finish()?;
    } else {
        // ── Video path (FLV / MP4) ──
        let keep_audio = !no_audio;
        let keep_video = !no_video;

        let download_path = if format == Format::Mp4 {
            dest.with_extension("tmp.flv")
        } else {
            dest.clone()
        };

        let mut downloader = DownloadRecorder::new(&download_path, keep_audio, keep_video);

        loop {
            if cancel_token.is_cancelled() {
                info!("Cancelled.");
                break;
            }

            let url = fetch_url(real_id, quality).await?;
            info!("Connecting (reconnect #{reconnect_count})...");

            match downloader.record_connection(&url, cancel_token).await {
                Ok(ConnEnd::Cancelled) => break,
                Ok(ConnEnd::Eof) => {
                    reconnect_count += 1;
                    if reconnect_count > max_reconnects {
                        error!("Too many reconnects ({max_reconnects})");
                        break;
                    }
                    if !still_live(real_id).await {
                        info!("Stream ended.");
                        break;
                    }
                    downloader.mark_reconnect();
                    backoff(reconnect_count).await;
                }
                Err(e) => {
                    error!("Download error: {e}");
                    reconnect_count += 1;
                    if reconnect_count > max_reconnects {
                        break;
                    }
                    downloader.mark_reconnect();
                    backoff(reconnect_count).await;
                }
            }
        }

        if format == Format::Mp4 {
            info!("Remuxing FLV -> MP4 via ffmpeg...");
            tokio::task::spawn_blocking({
                let tmp = download_path.clone();
                let dest = dest.clone();
                move || remux_flv_to_mp4(&tmp, &dest)
            })
            .await??;
            if let Err(e) = std::fs::remove_file(&download_path) {
                warn!("Failed to remove temp file: {e}");
            }
        }
    }

    info!("Recording saved to {}", dest.display());
    println!("Recording saved to {}", dest.display());
    Ok(())
}

async fn fetch_url(real_id: i64, quality: u32) -> Result<String> {
    loop {
        match live::get_stream_url(real_id, quality).await {
            Ok(url) => return Ok(url),
            Err(e) => {
                if !still_live(real_id).await {
                    anyhow::bail!("Stream ended while fetching URL");
                }
                warn!("URL fetch failed: {e}. Retrying...");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

async fn still_live(real_id: i64) -> bool {
    matches!(live::get_live_status(real_id).await, Ok(1 | 2))
}

async fn backoff(attempt: u32) {
    tokio::time::sleep(Duration::from_secs((1u64 << attempt.min(5)).min(30))).await;
}
