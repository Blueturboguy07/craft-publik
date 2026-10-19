//! The app's AI settings: which provider is chosen and the own key, in
//! `<config dir>/ai.json`, mode 0600. The publik key itself lives in publik's
//! credential file, never here.

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const PROVIDER_PUBLIK: &str = "publik";
pub const PROVIDER_OWN: &str = "own";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AiSettings {
    /// `publik`, `own`, or empty (not chosen yet).
    pub provider: String,
    pub own_key: String,
    pub own_base_url: String,
    pub own_model: String,
    /// The setup flow ran to an end (chosen, or dismissed with Later).
    pub setup_done: bool,
    /// The first-run card (balance, cost sentence, Sign in) has been shown once.
    pub first_card_shown: bool,
    /// The disclosure version the person accepted, 0 if never.
    pub disclosure_accepted: u32,
}

impl AiSettings {
    pub fn load(path: Option<&Path>) -> Self {
        let Some(p) = path else {
            return Self::default();
        };
        std::fs::read_to_string(p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: Option<&Path>) -> Result<(), String> {
        let Some(p) = path else { return Ok(()) };
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        crate::credential::write_private(p, json.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_files_load_with_defaults() {
        let s: AiSettings =
            serde_json::from_str(r#"{"provider":"own","own_key":"sk-x"}"#).unwrap_or_default();
        assert_eq!(s.provider, "own");
        assert!(!s.setup_done);
    }
}
