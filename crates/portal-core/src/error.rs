use thiserror::Error;

#[derive(Debug, Error)]
pub enum PortalError {
    #[error("unsupported operation: {0}")]
    Unsupported(&'static str),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error in {path}: {detail}")]
    Parse { path: String, detail: String },
    #[error("{0}")]
    Other(String),
}

// Tauri commands return Result<T, E> where E: Serialize; errors cross IPC as their display string.
impl serde::Serialize for PortalError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

pub type Result<T, E = PortalError> = std::result::Result<T, E>;
