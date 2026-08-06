use anyhow::Result;
use futures_util::StreamExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::{RwLock, broadcast};
use tracing::{debug, error, info, warn};

use crate::flv::FlvStripper;
use crate::live;

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Safari/605.1.15";
const REFERER: &str = "https://www.bilibili.com/";

pub async fn pipe(
    short_id: i64,
    quality: u32,
    timeout: Option<Duration>,
    bind_addr: Option<String>,
) -> Result<()> {
    let real_id = live::resolve_room_id(short_id).await?;

    let work = async {
        if let Some(addr) = bind_addr {
            pipe_listen(real_id, quality, &addr).await
        } else {
            pipe_stdout(real_id, quality).await
        }
    };

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

async fn pipe_stdout(real_id: i64, quality: u32) -> Result<()> {
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
                    Ok(0) => {
                        info!("Stream ended.");
                        break;
                    }
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
                            if let Err(e) = stdout.flush().await {
                                if e.kind() == std::io::ErrorKind::BrokenPipe {
                                    info!("Pipe closed by downstream, exiting.");
                                    return Ok(());
                                }
                                return Err(e.into());
                            }
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
            Ok(0) => {
                info!("Stream ended during reconnect.");
                break;
            }
            Ok(_) => {}
            Err(e) => warn!("Status check error before reconnect: {e}"),
        }

        tokio::time::sleep(Duration::from_secs(
            (1u64 << reconnect_count.min(5)).min(30),
        ))
        .await;
    }

    Ok(())
}

// ── TCP listen mode ──────────────────────────────────────────────────

/// Buffer the first N bytes of filtered FLV as metadata so late-joining
/// clients receive the FLV header and codec config they need to initialise.
const METADATA_LIMIT: usize = 65536;

async fn pipe_listen(real_id: i64, quality: u32, bind_addr: &str) -> Result<()> {
    info!("Room resolved to real ID {real_id}");

    live::wait_for_live(real_id).await?;

    let listener = TcpListener::bind(bind_addr).await?;
    let addr = listener.local_addr()?;
    info!("Listening on {addr} — connect with:  nc {addr} | ffplay -f flv -");

    let (tx, _) = broadcast::channel::<Arc<Vec<u8>>>(256);
    let metadata: Arc<RwLock<Vec<u8>>> = Arc::new(RwLock::new(Vec::with_capacity(METADATA_LIMIT)));
    let metadata_done = Arc::new(AtomicBool::new(false));

    // Spawn stream → broadcast feed
    {
        let tx = tx.clone();
        let metadata = metadata.clone();
        let metadata_done = metadata_done.clone();
        tokio::spawn(async move {
            if let Err(e) =
                stream_to_broadcast(real_id, quality, &tx, &metadata, &metadata_done).await
            {
                error!("Stream feed ended: {e}");
            }
        });
    }

    // Accept clients
    loop {
        let (mut stream, client_addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                error!("Accept error: {e}");
                return Ok(());
            }
        };
        info!("Client connected: {client_addr}");

        let meta_snapshot = metadata.read().await.clone();
        let mut rx = tx.subscribe();

        tokio::spawn(async move {
            if !meta_snapshot.is_empty() && stream.write_all(&meta_snapshot).await.is_err() {
                debug!("Client {client_addr} disconnected during metadata send");
                return;
            }
            loop {
                match rx.recv().await {
                    Ok(chunk) => {
                        if stream.write_all(&chunk).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            debug!("Client {client_addr} disconnected");
        });
    }
}

async fn stream_to_broadcast(
    real_id: i64,
    quality: u32,
    tx: &broadcast::Sender<Arc<Vec<u8>>>,
    metadata: &RwLock<Vec<u8>>,
    metadata_done: &AtomicBool,
) -> Result<()> {
    let mut stripper = FlvStripper::new();
    let mut reconnect_count = 0u32;
    let max_reconnects = 20;

    loop {
        let url = match live::get_stream_url(real_id, quality).await {
            Ok(url) => url,
            Err(e) => {
                match live::get_live_status(real_id).await {
                    Ok(1) => warn!("Stream is live but URL fetch failed: {e}. Retrying..."),
                    Ok(0) => {
                        info!("Stream ended.");
                        return Ok(());
                    }
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

        loop {
            tokio::select! {
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            let output = stripper.process(&bytes);
                            if output.is_empty() {
                                continue;
                            }

                            // Buffer metadata for late-joining clients
                            if !metadata_done.load(Ordering::Relaxed) {
                                let mut meta = metadata.write().await;
                                if meta.len() < METADATA_LIMIT {
                                    meta.extend_from_slice(output);
                                    if meta.len() >= METADATA_LIMIT {
                                        metadata_done.store(true, Ordering::SeqCst);
                                        info!("Metadata buffer complete ({} bytes)", meta.len());
                                    }
                                }
                            }

                            let _ = tx.send(Arc::new(output.to_vec()));
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
            Ok(0) => {
                info!("Stream ended during reconnect.");
                break;
            }
            Ok(_) => {}
            Err(e) => warn!("Status check error before reconnect: {e}"),
        }

        tokio::time::sleep(Duration::from_secs(
            (1u64 << reconnect_count.min(5)).min(30),
        ))
        .await;
    }

    Ok(())
}
