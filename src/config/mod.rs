use std::{path::PathBuf};
use eyre::Result;

pub mod info;
pub mod model;
pub mod parser;

/// Determine default config path
pub async fn get_config_path() -> Result<PathBuf> {
    if let Some(mut path) = dirs::home_dir() {
        path.push(".config/stasis/stasis.rune");
        if path.exists() {
            return Ok(path);
        }
    }
    let fallback = PathBuf::from("/etc/stasis/stasis.rune");
    if fallback.exists() {
        return Ok(fallback);
    }
    Err(eyre::eyre!("Could not find stasis configuration file"))
}
