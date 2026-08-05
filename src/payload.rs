use api_req::{Method, Payload};
use serde::Serialize;

// ── Auth ──

#[derive(Debug, Payload, Serialize)]
#[api_req(path = "/x/passport-login/web/qrcode/generate")]
pub struct QrPayload;

#[derive(Debug, Payload, Serialize)]
#[api_req(path = "/x/passport-login/web/qrcode/poll")]
pub struct QrPollPayload {
    pub qrcode_key: String,
}

#[derive(Debug, Payload, Serialize)]
#[api_req(path = "/x/web-interface/nav")]
pub struct NavPayload;

#[allow(non_snake_case)]
#[derive(Debug, Payload, Serialize)]
#[api_req(path = "/login/exit/v2", method = Method::POST, req = form)]
pub struct LogoutPayload {
    pub biliCSRF: String,
}

// ── Room / Live ──

#[derive(Debug, Payload, Serialize)]
#[api_req(path = "/room/v1/Room/room_init")]
pub struct RoomInitPayload {
    pub id: i64,
}

#[derive(Debug, Payload, Serialize)]
#[api_req(path = "/room/v1/Room/get_info")]
pub struct RoomInfoPayload {
    pub room_id: i64,
}

#[derive(Debug, Payload, Serialize)]
#[api_req(path = "/room/v1/Room/playUrl")]
pub struct PlayUrlPayload {
    pub cid: i64,
    pub platform: String,
    pub quality: u32,
}

#[derive(Debug, Payload, Serialize)]
#[api_req(path = "/xlive/web-room/v2/index/getRoomPlayInfo")]
pub struct RoomPlayInfoPayload {
    pub room_id: i64,
    pub protocol: String,
    pub format: String,
    pub codec: String,
    pub qn: u32,
    pub platform: String,
}
