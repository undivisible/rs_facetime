use thiserror::Error;

#[derive(Debug, Error)]
pub enum RsFacetimeError {
    #[error("rs_facetime requires macOS")]
    UnsupportedPlatform,

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("private-api: {0}")]
    PrivateApi(String),
}

pub type Result<T> = std::result::Result<T, RsFacetimeError>;
