use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::live;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Format {
    Wav,
    Mp3,
    Flac,
}

impl Format {
    pub fn codec_name(self) -> &'static str {
        match self {
            Format::Wav => "pcm_s16le",
            Format::Mp3 => "libmp3lame",
            Format::Flac => "flac",
        }
    }

    pub fn muxer_name(self) -> &'static str {
        match self {
            Format::Wav => "wav",
            Format::Mp3 => "mp3",
            Format::Flac => "flac",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Wav => "wav",
            Format::Mp3 => "mp3",
            Format::Flac => "flac",
        }
    }
}

fn output_path(real_room_id: i64, format: Format, output: Option<String>) -> Result<PathBuf> {
    if let Some(o) = output {
        return Ok(PathBuf::from(o));
    }
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let filename = format!("{}_{}.{}", real_room_id, ts, format.extension());
    Ok(PathBuf::from(filename))
}

/// Persistent recorder: encoder + muxer + output file survive across
/// reconnections.  Only the demuxer / decoder / resampler are recreated
/// per connection.
struct Recorder {
    encoder: ffmpeg_next::codec::encoder::audio::Encoder,
    output: ffmpeg_next::format::context::Output,
    #[allow(dead_code)]
    format: Format,
    #[allow(dead_code)]
    encoder_rate: i32,
}

impl Recorder {
    fn new(path: &std::path::Path, format: Format) -> Result<Self> {
        ffmpeg_next::init().context("Failed to initialize ffmpeg")?;

        let codec = ffmpeg_next::codec::encoder::find_by_name(format.codec_name())
            .context(format!("Codec '{}' not found", format.codec_name()))?;

        let ctx = ffmpeg_next::codec::context::Context::new_with_codec(codec);
        let mut audio_enc = ctx.encoder().audio()?;

        let rate = 48000;
        audio_enc.set_rate(rate);
        audio_enc.set_channel_layout(ffmpeg_next::ChannelLayout::STEREO);

        let sample_fmt = match format {
            Format::Wav => {
                ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Packed)
            }
            Format::Mp3 => {
                ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed)
            }
            Format::Flac => {
                ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Packed)
            }
        };
        audio_enc.set_format(sample_fmt);

        if matches!(format, Format::Mp3) {
            audio_enc.set_bit_rate(192_000);
        }

        let encoder = audio_enc.open_as(codec)?;

        let mut output = ffmpeg_next::format::output_as(
            path.to_str().context("invalid output path")?,
            format.muxer_name(),
        )?;

        let mut out_stream = output.add_stream(codec)?;
        out_stream.set_parameters(&encoder);
        out_stream.set_time_base((1, rate));

        output.write_header()?;

        info!(
            "Recorder initialised: {:?} -> {}",
            format,
            path.display()
        );

        Ok(Self {
            encoder,
            output,
            format,
            encoder_rate: rate,
        })
    }

    /// Process one connection (one stream URL).
    fn record_connection(
        &mut self,
        url: &str,
        cancel_token: &tokio_util::sync::CancellationToken,
    ) -> Result<ConnEnd> {
        info!("Opening stream: {}...", &url[..60.min(url.len())]);

        let mut ictx =
            ffmpeg_next::format::input(&url).context("Failed to open stream URL")?;

        let audio_stream = ictx
            .streams()
            .best(ffmpeg_next::media::Type::Audio)
            .context("No audio stream found in FLV")?;
        let audio_idx = audio_stream.index();

        let decoder_ctx =
            ffmpeg_next::codec::context::Context::from_parameters(audio_stream.parameters())?;
        let mut decoder = decoder_ctx.decoder().audio()?;

        info!(
            "Audio stream: {} Hz, {:?}, {:?}",
            decoder.rate(),
            decoder.channel_layout(),
            decoder.format(),
        );

        // Resample to match encoder expectations
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

        // Flush decoder
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

        // Flush encoder
        self.encoder.send_eof()?;
        let mut encoded = ffmpeg_next::codec::packet::Packet::empty();
        while self.encoder.receive_packet(&mut encoded).is_ok() {
            encoded.set_stream(0);
            encoded.write_interleaved(&mut self.output)?;
        }

        info!("Connection ended normally (EOF)");
        Ok(ConnEnd::Eof)
    }

    fn finish(&mut self) -> Result<()> {
        self.output.write_trailer()?;
        info!("Recording finished, trailer written.");
        Ok(())
    }
}

enum ConnEnd {
    Eof,
    Cancelled,
}

pub async fn record(
    short_id: i64,
    format: Format,
    output: Option<String>,
    quality: u32,
    timeout: Option<Duration>,
) -> Result<()> {
    let real_id = live::resolve_room_id(short_id).await?;
    info!("Room {short_id} resolved to real ID {real_id}");

    // Wait for the streamer to go live
    live::wait_for_live(real_id, timeout).await?;

    let path = output_path(real_id, format, output)?;
    let start_time = std::time::Instant::now();
    let cancel_token = tokio_util::sync::CancellationToken::new();

    // Handle Ctrl+C
    let ct = cancel_token.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Ctrl+C received, finishing…");
        ct.cancel();
    });

    let recorder = Arc::new(Mutex::new(Recorder::new(&path, format)?));
    let mut reconnect_count = 0u32;
    let max_reconnects = 20;

    loop {
        // Check timeout
        if let Some(timeout) = timeout {
            if start_time.elapsed() > timeout {
                info!("Timeout reached.");
                break;
            }
        }
        if cancel_token.is_cancelled() {
            info!("Cancelled.");
            break;
        }

        // Get stream URL
        let url = match live::get_stream_url(real_id, quality).await {
            Ok(url) => url,
            Err(e) => {
                match live::get_live_status(real_id).await {
                    Ok(1) => warn!("Still live but URL fetch failed: {e}. Retrying…"),
                    Ok(0) => {
                        info!("Stream ended.");
                        break;
                    }
                    Ok(s) => warn!("Unknown live_status={s}: {e}. Retrying…"),
                    Err(e2) => warn!("Status check failed: {e2}. Retrying…"),
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
        };

        info!("Connecting (reconnect #{reconnect_count})…");

        let rec = recorder.clone();
        let tok = cancel_token.clone();
        let url_clone = url.clone();

        let result = tokio::task::spawn_blocking(move || {
            let mut guard = rec.blocking_lock();
            guard.record_connection(&url_clone, &tok)
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
                match live::get_live_status(real_id).await {
                    Ok(0) => {
                        info!("Stream ended after EOF.");
                        break;
                    }
                    Ok(1) => {
                        info!("Stream still live, reconnecting…");
                    }
                    Ok(_) => {}
                    Err(e) => warn!("Status check error: {e}"),
                }
                let backoff =
                    Duration::from_secs((1 << reconnect_count.min(5)).min(30));
                tokio::time::sleep(backoff).await;
            }
        }
    }

    // Write trailer
    let rec = recorder.clone();
    tokio::task::spawn_blocking(move || rec.blocking_lock().finish()).await??;

    info!("Recording saved to {}", path.display());
    println!("Recording saved to {}", path.display());
    Ok(())
}
