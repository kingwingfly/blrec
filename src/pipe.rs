use anyhow::Result;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tracing::{error, info, warn};

use crate::flv::FlvStripper;
use crate::live;

pub async fn pipe(
    short_id: i64,
    quality: u32,
    timeout: Option<std::time::Duration>,
) -> Result<()> {
    let real_id = live::resolve_room_id(short_id).await?;
    info!("Room {} resolved to real ID {}", short_id, real_id);

    // Wait for the streamer to go live
    live::wait_for_live(real_id, timeout).await?;

    let start_time = std::time::Instant::now();
    let mut stripper = FlvStripper::new();
    let mut reconnect_count = 0u32;
    let max_reconnects = 20;

    loop {
        // Check timeout
        if let Some(timeout) = timeout
            && start_time.elapsed() > timeout {
                info!("Timeout reached, stopping.");
                break;
            }

        // Get fresh stream URL
        let url = match live::get_stream_url(real_id, quality).await {
            Ok(url) => url,
            Err(e) => {
                match live::get_live_status(real_id).await {
                    Ok(1) => {
                        warn!("Stream is live but URL fetch failed: {e}. Retrying...");
                    }
                    Ok(0) => {
                        info!("Stream ended.");
                        break;
                    }
                    Ok(_) => warn!("Could not determine status: {e}. Retrying..."),
                    Err(e) => warn!("Could not determine status: {e}. Retrying..."),
                }
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                continue;
            }
        };

        info!("Connecting to stream (reconnect #{reconnect_count})...");
        let client = reqwest::Client::new();
        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to connect: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                continue;
            }
        };

        let mut stream = resp.bytes_stream();
        let mut stdout = tokio::io::stdout();

        // Stream bytes to stdout, checking live status periodically
        loop {
            tokio::select! {
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            let output = stripper.process(&bytes);
                            if let Err(e) = stdout.write_all(output).await {
                                if e.kind() == std::io::ErrorKind::BrokenPipe {
                                    info!("Pipe closed by downstream process, exiting.");
                                    return Ok(());
                                }
                                return Err(e.into());
                            }
                            if let Err(e) = stdout.flush().await {
                                return Err(e.into());
                            }
                        }
                        Some(Err(e)) => {
                            error!("Stream error: {e}");
                            break; // reconnect
                        }
                        None => {
                            info!("Stream connection closed (EOF)");
                            break; // reconnect
                        }
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                    match live::get_live_status(real_id).await {
                        Ok(0) => {
                            info!("Streamer went offline, stopping.");
                            return Ok(());
                        }
                        Ok(_) => {} // still live or looping
                        Err(e) => warn!("Status check error: {e}"),
                    }
                    if let Some(timeout) = timeout
                        && start_time.elapsed() > timeout {
                            info!("Timeout reached.");
                            return Ok(());
                        }
                }
            }
        }

        // Reconnection logic
        reconnect_count += 1;
        if reconnect_count > max_reconnects {
            anyhow::bail!("Too many reconnects ({max_reconnects})");
        }
        stripper.mark_reconnect();

        // Check if still live before reconnecting
                match live::get_live_status(real_id).await {
                    Ok(0) => {
                        info!("Stream ended during reconnect.");
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => warn!("Status check error before reconnect: {e}"),
                }

        // Exponential backoff: 1s → 2s → 4s → 8s → 16s → 30s (max)
        let backoff =
            std::time::Duration::from_secs((1 << reconnect_count.min(5)).min(30));
        tokio::time::sleep(backoff).await;
    }

    Ok(())
}
