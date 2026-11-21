pub mod model;
pub mod scanner;
pub mod service;
pub mod store;

pub use model::*;
pub use scanner::{ScanOptions, scan_project};
pub use service::{Core, default_data_dir};
pub use store::Store;

/// Length-prefix identity components to avoid ambiguous joined paths.
pub fn stable_id(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

pub fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
