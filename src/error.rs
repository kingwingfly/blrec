use thiserror::Error;

#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum LiveError {
    #[error("room not found")]
    RoomNotFound,
    #[error("stream not live (status: {0})")]
    NotLive(i32),
    #[error("no stream URL available")]
    NoStreamUrl,
    #[error("auth failed: {0}")]
    AuthFailed(String),
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ffmpeg error: {0}")]
    Ffmpeg(#[from] ffmpeg_next::Error),
    #[error("timeout reached")]
    Timeout,
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}
