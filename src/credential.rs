//! The per-app credential file publik writes on this computer:
//! `~/Library/Application Support/publik/apps/<slug>.json` (macOS),
//! `%LOCALAPPDATA%\publik\apps\<slug>.json` (Windows),
//! `${XDG_CONFIG_HOME:-~/.config}/publik/apps/<slug>.json` (Linux). Mode 0600.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Credential {
    pub key: String,
    pub base_url: String,
    pub install_id: String,
    pub app_slug: String,
    pub claim_url: String,
    pub add_credit_url: String,
    pub claim_state: String,
    /// `disclosure.cost` from the provisioning reply, shown verbatim on the card.
    pub cost_sentence: String,
    pub balance_micros: Option<i64>,
    pub minted_at: String,
}

pub fn dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or(home.join("AppData/Local"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or(home.join(".config"))
    };
    Some(base.join("publik").join("apps"))
}

pub fn path(slug: &str) -> Option<PathBuf> {
    Some(dir()?.join(format!("{slug}.json")))
}

/// The credential on disk, if it holds a `pk_` key.
pub fn read(slug: &str) -> Option<Credential> {
    let raw = std::fs::read_to_string(path(slug)?).ok()?;
    let mut c: Credential = serde_json::from_str(&raw).ok()?;
    if !c.key.starts_with("pk_") {
        return None;
    }
    if c.app_slug.is_empty() {
        c.app_slug = slug.to_string();
    }
    Some(c)
}

/// Writes the file with the key before anything else sees it.
pub fn write(slug: &str, c: &Credential) -> Result<(), String> {
    let path = path(slug).ok_or("no home directory")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    let json = serde_json::to_string_pretty(c).map_err(|e| e.to_string())?;
    write_private(&path, json.as_bytes())
}

pub fn forget(slug: &str) {
    if let Some(p) = path(slug) {
        let _ = std::fs::remove_file(p);
    }
}

/// Writes a file readable by this user only (0600 on Unix; the user's profile ACL on Windows).
pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes).map_err(|e| e.to_string())
    }
}

/// 16 random bytes as a v4 UUID string: the mint's idempotency key. No crate:
/// the OS random source through `RandomState`, mixed over several draws.
pub fn new_install_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut b = [0u8; 16];
    for chunk in b.chunks_mut(8) {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        let v = h.finish().to_le_bytes();
        chunk.copy_from_slice(&v[..chunk.len()]);
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}{}{}{}-{}{}-{}{}-{}{}-{}{}{}{}{}{}",
        h[0],
        h[1],
        h[2],
        h[3],
        h[4],
        h[5],
        h[6],
        h[7],
        h[8],
        h[9],
        h[10],
        h[11],
        h[12],
        h[13],
        h[14],
        h[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_ids_are_v4_shaped_and_distinct() {
        let a = new_install_id();
        let b = new_install_id();
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
        assert_ne!(a, b);
    }
}
