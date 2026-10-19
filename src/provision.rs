//! `POST /installs`: mint this computer's key for this app. Called only after
//! the disclosure was accepted (the builder's self-test is its own consent).

use serde_json::{json, Value};

use crate::credential::{self, Credential};
use crate::{http, AppInfo, DEFAULT_BASE_URL, DISCLOSURE_VERSION};

fn os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

fn arch_name() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    }
}

fn os_version() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()?;
        let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return if v.is_empty() { None } else { Some(v) };
    }
    #[allow(unreachable_code)]
    None
}

fn token(app: &AppInfo) -> Option<String> {
    let runtime = std::env::var("PUBLIK_APP_TOKEN").unwrap_or_default();
    let t = if runtime.trim().is_empty() {
        app.app_token.trim()
    } else {
        runtime.trim()
    };
    if t.starts_with("pat_") {
        Some(t.to_string())
    } else {
        None
    }
}

fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Date arithmetic without a crate: days since epoch → civil date (Howard Hinnant's algorithm).
    let days = (secs / 86_400) as i64;
    let (h, m, s) = ((secs % 86_400) / 3600, (secs % 3600) / 60, secs % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Err is a readable message; offer the person's own key then.
pub fn provision(app: &AppInfo) -> Result<Credential, String> {
    let Some(token) = token(app) else {
        return Err("This build has no publik API app token. Use your own key for now.".into());
    };
    let base = crate::resolve_base_url("");
    let mut prior = credential::read(app.slug);
    let mut install_id = prior
        .as_ref()
        .map(|c| c.install_id.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(credential::new_install_id);
    for _ in 0..2 {
        let mut body = json!({
            "app_token": token,
            "app_slug": app.slug,
            "app_version": app.version,
            "os": os_name(),
            "arch": arch_name(),
            "install_id": install_id,
            "disclosure_version": DISCLOSURE_VERSION,
            "dialects": app.dialects,
        });
        if let Some(v) = os_version() {
            body["os_version"] = Value::String(v);
        }
        let res = http::post_json(&format!("{base}/installs"), None, &body, 25)?;
        let v = res.json();
        let key = v["key"].as_str().map(str::to_string).or_else(|| {
            if res.status == 200 {
                prior.as_ref().map(|c| c.key.clone())
            } else {
                None
            }
        });
        if let (200 | 201, Some(key)) = (res.status, key) {
            let c = Credential {
                key,
                base_url: v["base_url"]
                    .as_str()
                    .unwrap_or(DEFAULT_BASE_URL)
                    .to_string(),
                install_id: v["install_id"].as_str().unwrap_or(&install_id).to_string(),
                app_slug: app.slug.to_string(),
                claim_url: crate::publik_link(v["claim_url"].as_str()).unwrap_or_default(),
                add_credit_url: crate::publik_link(
                    v["wallet"]["add_credit_url"]
                        .as_str()
                        .or(v["add_credit_url"].as_str()),
                )
                .unwrap_or_default(),
                claim_state: v["claim_state"].as_str().unwrap_or("anonymous").to_string(),
                cost_sentence: v["disclosure"]["cost"]
                    .as_str()
                    .unwrap_or(crate::WHY_IT_COSTS)
                    .to_string(),
                balance_micros: v["balance_micros"].as_i64(),
                minted_at: now_rfc3339(),
            };
            credential::write(app.slug, &c)?;
            return Ok(c);
        }
        // A replayed install whose key is gone, or a revoked install: mint a fresh one once.
        if res.status == 200 || v["error"]["type"] == "install_revoked" {
            install_id = credential::new_install_id();
            prior = None;
            continue;
        }
        return Err(v["error"]["message"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "publik API could not set up this computer ({}). Use your own key for now.",
                    res.status
                )
            }));
    }
    Err("publik API could not set up this computer. Use your own key for now.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_has_the_right_shape() {
        let s = now_rfc3339();
        assert_eq!(s.len(), 20);
        assert!(s.ends_with('Z'));
        assert!(s.starts_with("20"));
    }
}
