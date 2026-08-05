use anyhow::Result;
use futures_util::StreamExt;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tracing::{error, info, warn};

use crate::flv::FlvStripper;
use crate::live;

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Safari/605.1.15";
const REFERER: &str = "https://www.bilibili.com/";

pub async fn pipe(
    short_id: i64,
    quality: u32,
    timeout: Option<Duration>,
) -> Result<()> {
    let real_id = live::resolve_room_id(short_id).await?;

    let work = pipe_inner(real_id, quality);
    if let Some(t) = timeout {
        match tokio::time::timeout(t, work).await {
            Ok(res) => res,
            Err(_elapsed) => {
                info!("Timeout reached, stopping.");
                Ok(())
            }
        }
    } else {
        work.await
    }
}

async fn pipe_inner(real_id: i64, quality: u32) -> Result<()> {
    info!("Room resolved to real ID {real_id}");

    live::wait_for_live(real_id).await?;

    let mut stripper = FlvStripper::new();
    let mut reconnect_count = 0u32;
    let max_reconnects = 20;

    loop {
        let url = match live::get_stream_url(real_id, quality).await {
            Ok(url) => url,
            Err(e) => {
                match live::get_live_status(real_id).await {
                    Ok(1) => warn!("Stream is live but URL fetch failed: {e}. Retrying..."),
                    Ok(0) => { info!("Stream ended."); break; }
                    Ok(_) => warn!("Could not determine status: {e}. Retrying..."),
                    Err(e) => warn!("Could not determine status: {e}. Retrying..."),
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
        };

        info!("Connecting to stream (reconnect #{reconnect_count})...");
        let client = reqwest::Client::new();
        let resp = match client
            .get(&url)
            .header("Referer", REFERER)
            .header("User-Agent", USER_AGENT)
            .send()
            .await
        {
            Ok(r) => {
                // Check for CDN error pages (status != 2xx)
                if !r.status().is_success() {
                    let status = r.status();
                    let body = r.text().await.unwrap_or_default();
                    error!("CDN returned {status}: {body:.200}");
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue;
                }
                r
            }
            Err(e) => {
                error!("Failed to connect: {e}");
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
        };

        let mut stream = resp.bytes_stream();
        let mut stdout = tokio::io::stdout();

        loop {
            tokio::select! {
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            let output = stripper.process(&bytes);
                            if let Err(e) = stdout.write_all(output).await {
                                if e.kind() == std::io::ErrorKind::BrokenPipe {
                                    info!("Pipe closed by downstream, exiting.");
                                    return Ok(());
                                }
                                return Err(e.into());
                            }
                            stdout.flush().await?;
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
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    match live::get_live_status(real_id).await {
                        Ok(0) => {
                            info!("Streamer went offline, stopping.");
                            return Ok(());
                        }
                        Ok(_) => {}
                        Err(e) => warn!("Status check error: {e}"),
                    }
                }
            }
        }

        reconnect_count += 1;
        if reconnect_count > max_reconnects {
            anyhow::bail!("Too many reconnects ({max_reconnects})");
        }
        stripper.mark_reconnect();

        match live::get_live_status(real_id).await {
            Ok(0) => { info!("Stream ended during reconnect."); break; }
            Ok(_) => {}
            Err(e) => warn!("Status check error before reconnect: {e}"),
        }

        tokio::time::sleep(Duration::from_secs(
            (1u64 << reconnect_count.min(5)).min(30),
        )).await;
    }

    Ok(())
}
