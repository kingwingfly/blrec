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
            Format::Flv => "flv",
        }
    }

    pub fn from_ext(ext: &str) -> Option<Self> {
        match ext {
            "wav" => Some(Format::Wav),
            "mp3" => Some(Format::Mp3),
            "flac" => Some(Format::Flac),
            "flv" => Some(Format::Flv),
            _ => None,
        }
    }

    fn codec_name(self) -> &'static str {
        match self {
            Format::Wav => "pcm_s16le",
            Format::Mp3 => "libmp3lame",
            Format::Flac => "flac",
            Format::Flv => unreachable!(),
        }
    }

    fn muxer_name(self) -> &'static str {
        match self {
            Format::Wav => "wav",
            Format::Mp3 => "mp3",
            Format::Flac => "flac",
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

// ── Audio FIFO ──────────────────────────────────────────────────────

/// Re-cuts resampled audio into the exact frame size an encoder demands.
///
/// The resampler emits frames whose length follows the input, but libmp3lame
/// (1152) and flac (4608) reject any frame that is not exactly `frame_size`
/// samples — the first short frame is taken as the last one and every frame
/// after it fails with `EINVAL`. Buffering here decouples the two sides.
///
/// Samples are held as raw bytes, one buffer per plane, which covers both
/// packed layouts (a single interleaved plane) and planar ones (one plane per
/// channel) without caring about the concrete sample type.
/// The channel layout is deliberately not held here: `ChannelLayout` wraps raw
/// pointers and so is not `Send`, which would keep the recorder off
/// `spawn_blocking`. It is passed back in at `pop()` time instead.
struct AudioFifo {
    planes: Vec<Vec<u8>>,
    format: ffmpeg_next::format::Sample,
    rate: u32,
    /// Bytes occupied by one sample within a single plane.
    stride: usize,
}

impl AudioFifo {
    fn new(format: ffmpeg_next::format::Sample, channels: usize, rate: u32) -> Self {
        let channels = channels.max(1);
        let (planes, stride) = if format.is_planar() {
            (channels, format.bytes())
        } else {
            (1, format.bytes() * channels)
        };
        Self {
            planes: vec![Vec::new(); planes],
            format,
            rate,
            stride,
        }
    }

    fn samples(&self) -> usize {
        self.planes[0].len() / self.stride
    }

    fn push(&mut self, frame: &ffmpeg_next::frame::Audio) {
        // `planes()` reports 0 for an empty frame, so `data()` would panic.
        if frame.samples() == 0 {
            return;
        }
        let valid = frame.samples() * self.stride;
        for (i, buf) in self.planes.iter_mut().enumerate() {
            // `data()` spans the whole allocation; only the leading `valid`
            // bytes hold samples, the rest is alignment padding.
            buf.extend_from_slice(&frame.data(i)[..valid]);
        }
    }

    fn pop(
        &mut self,
        samples: usize,
        layout: ffmpeg_next::ChannelLayout,
    ) -> ffmpeg_next::frame::Audio {
        let bytes = samples * self.stride;
        let mut frame = ffmpeg_next::frame::Audio::new(self.format, samples, layout);
        frame.set_rate(self.rate);
        for (i, buf) in self.planes.iter_mut().enumerate() {
            frame.data_mut(i)[..bytes].copy_from_slice(&buf[..bytes]);
            buf.drain(..bytes);
        }
        frame
    }
}

// ── Audio encoder (wav / mp3 / flac) ────────────────────────────────

struct AudioRecorder {
    encoder: ffmpeg_next::codec::encoder::audio::Encoder,
    output: ffmpeg_next::format::context::Output,
    fifo: AudioFifo,
    frame_size: usize,
    /// Presentation timestamp of the next encoder frame, in samples.
    next_pts: i64,
    enc_time_base: ffmpeg_next::Rational,
    stream_time_base: ffmpeg_next::Rational,
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
        a_enc.set_time_base((1, rate));

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

        // Muxers may rewrite the stream time base while writing the header, so
        // read it back rather than assuming the value set above survived.
        let stream_time_base = output
            .stream(0)
            .context("output stream missing")?
            .time_base();

        // PCM encoders take any frame size and report 0; pick a chunk for them
        // so every format goes through the same batching path.
        let frame_size = match encoder.frame_size() {
            0 => 1024,
            n => n as usize,
        };

        let fifo = AudioFifo::new(
            encoder.format(),
            encoder.channel_layout().channels().max(1) as usize,
            rate as u32,
        );

        Ok(Self {
            encoder,
            output,
            fifo,
            frame_size,
            next_pts: 0,
            enc_time_base: (1, rate).into(),
            stream_time_base,
        })
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

        let mut end = ConnEnd::Eof;
        for (stream, packet) in ictx.packets() {
            if cancel_token.is_cancelled() {
                end = ConnEnd::Cancelled;
                break;
            }
            if stream.index() != audio_idx {
                continue;
            }
            decoder.send_packet(&packet)?;
            self.drain_decoder(&mut decoder, &mut resampler)?;
        }

        // Flush the decoder, then the resampler's own buffer — without this the
        // tail of every connection is silently dropped.
        decoder.send_eof()?;
        self.drain_decoder(&mut decoder, &mut resampler)?;
        self.flush_resampler(&mut resampler)?;

        // The encoder is *not* flushed here: it is reused across reconnects and
        // may only be closed once, in `finish()`.
        Ok(end)
    }

    /// Pull every frame the decoder will give up, resample it, and buffer it.
    fn drain_decoder(
        &mut self,
        decoder: &mut ffmpeg_next::codec::decoder::Audio,
        resampler: &mut ffmpeg_next::software::resampling::Context,
    ) -> Result<()> {
        let mut decoded = ffmpeg_next::frame::Audio::empty();
        while decoder.receive_frame(&mut decoded).is_ok() {
            let in_rate = decoded.rate().max(1) as usize;
            let out_rate = self.encoder.rate() as usize;

            // The output frame must be allocated by us: left empty, `run()`
            // sizes it from the *input* sample count, which truncates on every
            // call when upsampling (44.1 kHz → 48 kHz loses ~8%). `delay()` is
            // already expressed in output samples, so it is not rescaled.
            let pending = resampler.delay().map_or(0, |d| d.output).max(0) as usize;
            let capacity = pending + (decoded.samples() * out_rate).div_ceil(in_rate) + 64;

            let mut resampled = ffmpeg_next::frame::Audio::new(
                self.encoder.format(),
                capacity,
                self.encoder.channel_layout(),
            );
            resampler.run(&decoded, &mut resampled)?;
            self.fifo.push(&resampled);
            self.encode_buffered(false)?;
        }
        Ok(())
    }

    /// Drain samples still held inside the resampler at end of connection.
    fn flush_resampler(
        &mut self,
        resampler: &mut ffmpeg_next::software::resampling::Context,
    ) -> Result<()> {
        loop {
            let pending = resampler.delay().map_or(0, |d| d.output).max(0) as usize;
            if pending == 0 {
                break;
            }
            let mut resampled = ffmpeg_next::frame::Audio::new(
                self.encoder.format(),
                pending + 64,
                self.encoder.channel_layout(),
            );
            resampler.flush(&mut resampled)?;
            if resampled.samples() == 0 {
                break;
            }
            self.fifo.push(&resampled);
        }
        self.encode_buffered(false)
    }

    /// Encode whole `frame_size` chunks out of the FIFO. When `drain` is set,
    /// also emit whatever is left as a final short frame — permitted only once,
    /// as the very last frame handed to the encoder.
    fn encode_buffered(&mut self, drain: bool) -> Result<()> {
        while self.fifo.samples() >= self.frame_size || (drain && self.fifo.samples() > 0) {
            let samples = self.frame_size.min(self.fifo.samples());
            let mut frame = self.fifo.pop(samples, self.encoder.channel_layout());
            frame.set_pts(Some(self.next_pts));
            self.next_pts += samples as i64;

            self.encoder.send_frame(&frame)?;
            self.write_packets()?;
        }
        Ok(())
    }

    fn write_packets(&mut self) -> Result<()> {
        let mut encoded = ffmpeg_next::codec::packet::Packet::empty();
        while self.encoder.receive_packet(&mut encoded).is_ok() {
            encoded.set_stream(0);
            encoded.rescale_ts(self.enc_time_base, self.stream_time_base);
            encoded.write_interleaved(&mut self.output)?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        self.encode_buffered(true)?;
        self.encoder.send_eof()?;
        self.write_packets()?;
        self.output.write_trailer()?;
        Ok(())
    }
}

// ── Download helper ──────────────────────────────────────────────────

async fn download_connection(
    filter: &mut FlvFilter,
    url: &str,
    cancel_token: &tokio_util::sync::CancellationToken,
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<ConnEnd> {
    let client = reqwest::Client::new();
    let resp = client
        .get(url)
        .header("Referer", REFERER)
        .header("User-Agent", USER_AGENT)
        .send()
        .await?;

    let mut stream = resp.bytes_stream();

    loop {
        if cancel_token.is_cancelled() {
            return Ok(ConnEnd::Cancelled);
        }
        tokio::select! {
            chunk = stream.next() => {
                match chunk {
                    Some(Ok(bytes)) => {
                        let output = filter.process(&bytes);
                        writer.write_all(output).await?;
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

    record_inner(
        real_id,
        format,
        output,
        quality,
        no_video,
        no_audio,
        &cancel_token,
    )
    .await
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
    // Wait for the stream to start, but bail out on Ctrl+C / timeout.
    tokio::select! {
        result = live::wait_for_live(real_id) => result?,
        _ = cancel_token.cancelled() => {
            info!("Cancelled while waiting for stream to start.");
            return Ok(());
        }
    }

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
        // ── FLV path ──
        let keep_audio = !no_audio;
        let keep_video = !no_video;

        let mut filter = FlvFilter::new(keep_audio, keep_video);

        loop {
            if cancel_token.is_cancelled() {
                info!("Cancelled.");
                break;
            }

            let url = fetch_url(real_id, quality).await?;
            info!("Connecting (reconnect #{reconnect_count})...");

            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&dest)
                .await?;

            match download_connection(&mut filter, &url, cancel_token, &mut file).await {
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
                    filter.mark_reconnect();
                    backoff(reconnect_count).await;
                }
                Err(e) => {
                    error!("Download error: {e}");
                    reconnect_count += 1;
                    if reconnect_count > max_reconnects {
                        break;
                    }
                    filter.mark_reconnect();
                    backoff(reconnect_count).await;
                }
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
