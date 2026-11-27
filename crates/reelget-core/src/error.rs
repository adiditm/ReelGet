use thiserror::Error;

pub type Result<T> = std::result::Result<T, ReelGetError>;

#[derive(Debug, Error)]
pub enum ReelGetError {
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Unsupported platform or URL: {0}")]
    UnsupportedPlatform(String),

    #[error("Authentication required for {platform}: {hint}")]
    AuthenticationRequired { platform: String, hint: String },

    #[error("DRM-protected content is not supported: {0}")]
    DrmProtected(String),

    #[error("No downloadable media found: {0}")]
    MediaNotFound(String),

    #[error("Extractor failed ({platform}): {message}")]
    ExtractorFailed { platform: String, message: String },

    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("HTTP status {status} for {url}")]
    HttpStatus { status: u16, url: String },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HLS playlist parse error: {0}")]
    HlsParse(String),

    #[error("Decryption error: {0}")]
    Crypto(String),

    #[error("Muxer error: {0}")]
    Muxer(String),

    #[error("Invalid clip range: {0}")]
    InvalidClipRange(String),

    #[error("Download cancelled")]
    Cancelled,
}
