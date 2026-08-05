use anyhow::Result;
use api_req::ApiCaller as _;
use tracing::{info, warn};

use crate::api::LiveApi;
use crate::payload::{PlayUrlPayload, RoomInfoPayload, RoomInitPayload, RoomPlayInfoPayload};
use crate::response::{PlayUrlResp, RoomInfoResp, RoomInitResp, RoomPlayInfoResp};

/// Resolve a short room ID to the real (long) room ID.
pub async fn resolve_room_id(short_id: i64) -> Result<i64> {
    let resp: RoomInitResp = LiveApi::request(RoomInitPayload { id: short_id }).await?;
    if resp.code != 0 {
        anyhow::bail!("Room not found (code={})", resp.code);
    }
    info!(
        "Resolved room {} -> real room {}",
        short_id, resp.data.room_id
    );
    Ok(resp.data.room_id)
}

/// Get live status: 0 = offline, 1 = live, 2 = looping.
pub async fn get_live_status(real_room_id: i64) -> Result<i32> {
    let resp: RoomInfoResp =
        LiveApi::request(RoomInfoPayload { room_id: real_room_id }).await?;
    if resp.code != 0 {
        anyhow::bail!("Failed to get room info (code={})", resp.code);
    }
    Ok(resp.data.live_status)
}

/// Get the HTTP-FLV stream URL for a room.
/// Tries the modern API first, falls back to the legacy API.
pub async fn get_stream_url(real_room_id: i64, qn: u32) -> Result<String> {
    // Try modern API first
    if let Ok(url) = get_stream_url_modern(real_room_id, qn).await
        && !url.is_empty() {
            return Ok(url);
        }
    // Fallback to legacy API
    get_stream_url_legacy(real_room_id, qn).await
}

async fn get_stream_url_modern(real_room_id: i64, qn: u32) -> Result<String> {
    let resp: RoomPlayInfoResp = LiveApi::request(RoomPlayInfoPayload {
        room_id: real_room_id,
        protocol: "0".into(), // http_stream (FLV)
        format: "0".into(),   // flv
        codec: "0".into(),    // AVC
        qn,
        platform: "web".into(),
    })
    .await?;

    if resp.code != 0 {
        anyhow::bail!("getRoomPlayInfo failed (code={})", resp.code);
    }

    let playurl = &resp.data.playurl_info.playurl;
    for stream in &playurl.stream {
        if stream.protocol_name == "http_stream" {
            for fmt in &stream.format {
                if fmt.format_name == "flv" {
                    for codec in &fmt.codec {
                        if let Some(url_info) = codec.url_info.first() {
                            let url =
                                format!("{}{}{}", url_info.host, codec.base_url, url_info.extra);
                            info!(
                                "Got stream URL (modern): {}...",
                                &url[..60.min(url.len())]
                            );
                            return Ok(url);
                        }
                    }
                }
            }
        }
    }
    anyhow::bail!("No playable FLV stream found in modern API response")
}

async fn get_stream_url_legacy(real_room_id: i64, qn: u32) -> Result<String> {
    let resp: PlayUrlResp = LiveApi::request(PlayUrlPayload {
        cid: real_room_id,
        platform: "web".into(),
        quality: qn,
    })
    .await?;

    if resp.code != 0 {
        anyhow::bail!("playUrl failed (code={})", resp.code);
    }

    if let Some(durl) = resp.data.durl.first() {
        info!(
            "Got stream URL (legacy): {}...",
            &durl.url[..60.min(durl.url.len())]
        );
        Ok(durl.url.clone())
    } else {
        anyhow::bail!("No stream URLs in legacy API response")
    }
}

/// Poll until the streamer goes live (status == 1).
pub async fn wait_for_live(real_room_id: i64) -> Result<()> {
    loop {
        match get_live_status(real_room_id).await {
            Ok(1) => {
                info!("Stream is now live!");
                return Ok(());
            }
            Ok(0) => {
                info!("Stream offline, waiting...");
            }
            Ok(s) => {
                warn!("Unknown live_status={s}, waiting...");
            }
            Err(e) => {
                warn!("Error checking status: {e}, retrying...");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}
