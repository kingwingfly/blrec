use serde::Deserialize;
use url::Url;

// ── QR ──

#[derive(Debug, Deserialize)]
pub struct QrResp {
    pub data: QrData,
}

#[derive(Debug, Deserialize)]
pub struct QrData {
    pub qrcode_key: String,
    pub url: Url,
}

#[derive(Debug, Deserialize)]
pub struct QrPollResp {
    pub data: QrPollData,
}

#[derive(Debug, Deserialize)]
pub struct QrPollData {
    pub code: u32,
    pub message: String,
}

// ── Nav (user info) ──

#[derive(Debug, Deserialize)]
pub struct NavResp {
    pub code: i32,
    pub data: NavData,
}

#[derive(Debug, Deserialize)]
pub struct NavData {
    pub mid: Option<i64>,
    pub uname: Option<String>,
}

// ── Logout ──

#[derive(Debug, Deserialize)]
pub struct LogoutResp {
    pub code: i32,
    pub message: Option<String>,
}

// ── Room init ──

#[derive(Debug, Deserialize)]
pub struct RoomInitResp {
    pub code: i32,
    pub data: RoomInitData,
}

#[derive(Debug, Deserialize)]
pub struct RoomInitData {
    pub room_id: i64,
}

// ── Room info ──

#[derive(Debug, Deserialize)]
pub struct RoomInfoResp {
    pub code: i32,
    pub data: RoomInfoData,
}

#[derive(Debug, Deserialize)]
pub struct RoomInfoData {
    #[serde(default)]
    pub live_status: i32,
}

// ── Play URL (legacy) ──

#[derive(Debug, Deserialize)]
pub struct PlayUrlResp {
    pub code: i32,
    pub data: PlayUrlData,
}

#[derive(Debug, Deserialize)]
pub struct PlayUrlData {
    #[serde(default)]
    pub durl: Vec<Durl>,
}

#[derive(Debug, Deserialize)]
pub struct Durl {
    pub url: String,
}

// ── Modern play info ──

#[derive(Debug, Deserialize)]
pub struct RoomPlayInfoResp {
    pub code: i32,
    pub data: RoomPlayInfoData,
}

#[derive(Debug, Deserialize)]
pub struct RoomPlayInfoData {
    pub playurl_info: PlayUrlInfo,
}

#[derive(Debug, Deserialize)]
pub struct PlayUrlInfo {
    pub playurl: PlayUrl,
}

#[derive(Debug, Deserialize)]
pub struct PlayUrl {
    #[serde(default)]
    pub stream: Vec<PlayStream>,
}

#[derive(Debug, Deserialize)]
pub struct PlayStream {
    pub protocol_name: String,
    #[serde(default)]
    pub format: Vec<PlayFormat>,
}

#[derive(Debug, Deserialize)]
pub struct PlayFormat {
    pub format_name: String,
    #[serde(default)]
    pub codec: Vec<PlayCodec>,
}

#[derive(Debug, Deserialize)]
pub struct PlayCodec {
    pub base_url: String,
    #[serde(default)]
    pub url_info: Vec<UrlInfo>,
}

#[derive(Debug, Deserialize)]
pub struct UrlInfo {
    pub host: String,
    pub extra: String,
}
