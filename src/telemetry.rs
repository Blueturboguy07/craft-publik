//! Usage counting (publikhq.com/skills/telemetry.md): `app_opened` once the gate passed, then
//! `active_session` every 30 minutes, with this computer's publik key, so a signed-in person is
//! counted once across every app and computer. Two events, no content. Send and forget: a
//! failure never reaches the person, and a 404 (the endpoint not deployed yet) is ignored.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{json, Value};

use crate::{credential, AppInfo};

static STARTED: AtomicBool = AtomicBool::new(false);

/// Off with `DO_NOT_TRACK` (anything but empty, `0` or `false`), `PUBLIK_TELEMETRY` set to `0`,
/// `false` or `off`, or `"usage": false` in `<publik dir>/consent.json`.
pub fn usage_off() -> bool {
    let env = |n: &str| std::env::var(n).unwrap_or_default().trim().to_lowercase();
    if !["", "0", "false"].contains(&env("DO_NOT_TRACK").as_str())
        || ["0", "false", "off"].contains(&env("PUBLIK_TELEMETRY").as_str())
    {
        return true;
    }
    credential::publik_dir()
        .and_then(|d| std::fs::read_to_string(d.join("consent.json")).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .map(|v| v["usage"] == false)
        .unwrap_or(false)
}

/// One random UUID per computer account, shared by every publik app, in
/// `<publik dir>/telemetry.json` (mode 0600).
fn install_id() -> String {
    let Some(dir) = credential::publik_dir() else {
        return credential::new_install_id();
    };
    let file = dir.join("telemetry.json");
    let saved = || {
        std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["install_id"].as_str().map(str::to_lowercase))
            .filter(|id| credential::is_uuid_v4(id))
    };
    if let Some(id) = saved() {
        return id;
    }
    // If the file cannot be saved, this ID lasts for this run only.
    let fresh = credential::new_install_id();
    let _ = std::fs::create_dir_all(&dir);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true); // never over another app's file
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&file) {
        Ok(mut f) => {
            let _ = writeln!(
                f,
                "{}",
                json!({ "install_id": fresh, "created_at": crate::provision::now_rfc3339() })
            );
            fresh
        }
        // Another publik app saved its ID first.
        Err(_) => saved().unwrap_or(fresh),
    }
}

fn send_event(app: &AppInfo, event: &str, install_id: &str) {
    let debug = std::env::var("PUBLIK_TELEMETRY_DEBUG").as_deref() == Ok("1");
    let say = |result: &str| {
        if debug {
            eprintln!("publik-telemetry: {event} -> {result}");
        }
    };
    if usage_off() {
        say("off (opt-out)");
        return;
    }
    let key = std::env::var("PUBLIK_API_KEY")
        .ok()
        .map(|k| k.trim().to_string())
        .or_else(|| credential::read(app.slug).map(|c| c.key))
        .filter(|k| k.starts_with("pk_"));
    let mut body = json!({
        "v": 1,
        "app": app.slug.trim().to_lowercase(),
        "install_id": install_id,
        "event": event,
        "app_version": app.version.trim().trim_start_matches(['v', 'V']),
        "os": crate::provision::os_name(),
    });
    match std::env::consts::ARCH {
        "aarch64" => body["arch"] = json!("arm64"),
        "x86_64" => body["arch"] = json!("x64"),
        _ => {}
    }
    let url = std::env::var("PUBLIK_TELEMETRY_URL")
        .unwrap_or_else(|_| crate::api_url(app.endpoints.telemetry));
    match crate::http::post_json(&url, key.as_deref(), &body, 5) {
        Ok(res) => say(&res.status.to_string()),
        Err(e) => say(&e),
    }
}

/// Called by the gate once it passed: `app_opened` now, `active_session` every 30 minutes, on a
/// background thread. Once per process; never panics the app.
pub fn start(app: &AppInfo) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    // Tests only.
    let s: u64 = std::env::var("PUBLIK_TELEMETRY_INTERVAL_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let interval = Duration::from_secs(if (1..=86_400).contains(&s) { s } else { 1_800 });
    let app = app.clone();
    std::thread::spawn(move || {
        let id = install_id();
        send_event(&app, app.endpoints.opened, &id);
        loop {
            std::thread::sleep(interval);
            send_event(&app, app.endpoints.active, &id);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_events_and_the_path_are_the_contracts() {
        let e = crate::Endpoints::DEFAULT;
        assert_eq!((e.opened, e.active), ("app_opened", "active_session"));
        assert!(crate::api_url(e.telemetry).ends_with("/api/v1/telemetry"));
    }
}
