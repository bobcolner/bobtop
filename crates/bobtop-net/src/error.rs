use thiserror::Error;

pub type Result<T> = std::result::Result<T, NetError>;

#[derive(Debug, Error)]
pub enum NetError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error in {context}: {message}")]
    Parse { context: &'static str, message: String },

    #[error("not supported on this platform")]
    Unsupported,

    #[error("missing capability: {0}")]
    MissingCapability(&'static str),

    #[error("backend `{backend}` failed: {source}")]
    Backend {
        backend: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("{0}")]
    Other(String),
}

impl NetError {
    pub fn parse(context: &'static str, message: impl Into<String>) -> Self {
        Self::Parse {
            context,
            message: message.into(),
        }
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::Other(message.into())
    }
}
