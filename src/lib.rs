//! publik API as one AI provider option for a Craft app.
//!
//! What this crate owns: the per-app credential file publik writes on this
//! computer, provisioning (`POST /installs` with the app's public token), the
//! OpenAI-compatible calls (chat, images, transcription, speech), the person's
//! own-key path, and (behind the `egui` feature) the setup flow every Craft app
//! shows on first launch and in its preferences.
//!
//! Resolution order (publikhq.com/sdk/publik.rs.md): the person's own key >
//! `PUBLIK_API_KEY` / `PUBLIK_API_BASE_URL` in the environment > this app's
//! credential file > provision on first launch, after the disclosure is
//! accepted > the own key again if provisioning fails.
//!
//! Copy rule (publik contract): "publik API", dollars, never tokens, never
//! "credits" as a unit, never the model vendor's name next to a price.
//!
//! HTTP goes through the system `curl` (macOS, Windows 10+ and Linux ship it),
//! so no TLS crate joins the app. A key only ever rides a curl config on stdin,
//! never argv.

pub mod client;
pub mod credential;
pub mod http;
pub mod own_key;
pub mod provision;
pub mod settings;
#[cfg(feature = "egui")]
pub mod ui;

use std::path::PathBuf;

pub use client::{Fail, Reply};
pub use credential::Credential;
pub use settings::AiSettings;

pub const DEFAULT_BASE_URL: &str = "https://publikhq.com/api/v1";
pub const DASHBOARD_URL: &str = "https://publikhq.com/dashboard/api";
/// Tier aliases. Ask for a tier, never a vendor model name.
pub const MODEL_FAST: &str = "publik-fast";
pub const MODEL_BALANCED: &str = "publik-balanced";
pub const MODEL_SMART: &str = "publik-smart";
/// The image, transcription and speech lines have no tier alias; these are the
/// names the gateway's price sheet carries for them.
pub const IMAGE_MODEL: &str = "gpt-image-1";
pub const TRANSCRIBE_MODEL: &str = "whisper-1";
pub const SPEECH_MODEL: &str = "tts-1";
pub const DISCLOSURE_VERSION: u32 = 1;

/// The two-sentence disclosure shown before provisioning. Never promises a starter.
pub const DISCLOSURE: &str = "This app can use publik API for its AI features: no key to paste, priced per use in dollars from a publik balance that starts at $0.00. Linking this computer to a publik account gives $0.05 of free use, once. Your prompts go through publik's servers to the model; you can switch to your own key at any time.";
pub const DISCLOSURE_DATA: &str = "What you send to an AI feature goes through publik's servers to a shared model account. publik never trains on it and does not store it. Everything else in this app stays on this computer.";
/// The one-sentence justification shown wherever money is mentioned, when the
/// provisioning reply did not carry its own.
pub const WHY_IT_COSTS: &str = "The model's provider charges per use. publik charges a fixed, published price that sits above what the model costs publik; the difference keeps publik running and pays the developer who built this app. Nothing is charged behind your back, and every call is visible on your publik dashboard.";
pub const CTA_SIGN_IN: &str = "Sign in with publik";
pub const CTA_ADD_PLAN: &str = "Add a plan or pack";
pub const CTA_LATER: &str = "Later";

/// What the host app tells this crate about itself.
#[derive(Clone, Debug)]
pub struct AppInfo {
    /// publik listing slug (`gridcraft`). Also the credential file's name.
    pub slug: &'static str,
    /// Display name (`GridCraft`).
    pub name: &'static str,
    /// `env!("CARGO_PKG_VERSION")`.
    pub version: &'static str,
    /// The app's public `pat_` token, compiled in from `publik-app-token.txt`.
    /// Empty means this build has no publik option; the own key still works.
    pub app_token: &'static str,
    /// The gateway dialects this app uses, reported at provisioning
    /// (`chat_completions`, `images`, `audio_transcriptions`, `audio_speech`).
    pub dialects: &'static [&'static str],
    /// Where the app keeps its settings; `ai.json` is written there, mode 0600.
    pub config_dir: Option<PathBuf>,
}

impl AppInfo {
    pub fn can_provision(&self) -> bool {
        let runtime = std::env::var("PUBLIK_APP_TOKEN").unwrap_or_default();
        let token = if runtime.trim().is_empty() {
            self.app_token
        } else {
            runtime.as_str()
        };
        token.trim().starts_with("pat_")
    }

    pub fn settings_path(&self) -> Option<PathBuf> {
        self.config_dir.as_ref().map(|d| d.join("ai.json"))
    }
}

/// Which key a call goes out with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Provider {
    /// The person's own key at an OpenAI-compatible endpoint.
    Own {
        service: own_key::Service,
        base_url: String,
        api_key: String,
        model: String,
    },
    /// A publik key (environment or credential file).
    Publik {
        base_url: String,
        api_key: String,
        model: String,
    },
    /// Nothing usable yet: the setup flow has not run, or was declined.
    NotSetUp,
}

impl Provider {
    pub fn is_publik(&self) -> bool {
        matches!(self, Self::Publik { .. })
    }
    pub fn is_set_up(&self) -> bool {
        !matches!(self, Self::NotSetUp)
    }
    /// A short line for status bars: never the key, never a vendor name next to a price.
    pub fn label(&self) -> String {
        match self {
            Self::Own { service, model, .. } => {
                format!("Your own key ({}) · {model}", service.name())
            }
            Self::Publik { model, .. } => format!("publik API · {model}"),
            Self::NotSetUp => "AI is not set up".to_string(),
        }
    }
    /// Whether this provider can serve a non-chat line (images, audio).
    pub fn supports(&self, line: Line) -> bool {
        match self {
            Self::Publik { .. } => true,
            Self::Own { service, .. } => match line {
                Line::Chat => true,
                Line::Images | Line::Transcribe | Line::Speech => service.has_media_lines(),
            },
            Self::NotSetUp => false,
        }
    }
}

/// The gateway lines a feature may need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line {
    Chat,
    Images,
    Transcribe,
    Speech,
}

impl Line {
    pub fn dialect(self) -> &'static str {
        match self {
            Self::Chat => "chat_completions",
            Self::Images => "images",
            Self::Transcribe => "audio_transcriptions",
            Self::Speech => "audio_speech",
        }
    }
}

/// `PUBLIK_API_BASE_URL` (dev) > the provisioning reply's `base_url` > the default.
pub fn resolve_base_url(from_credential: &str) -> String {
    if let Some(u) = std::env::var("PUBLIK_API_BASE_URL")
        .ok()
        .and_then(|s| trim_url(&s))
    {
        return u;
    }
    trim_url(from_credential).unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
}

fn trim_url(s: &str) -> Option<String> {
    let t = s.trim().trim_end_matches('/');
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// The provider in force, by the resolution order above. Pure: reads files, never the network.
pub fn resolve(app: &AppInfo, settings: &AiSettings) -> Provider {
    if settings.provider == settings::PROVIDER_OWN {
        let key = settings.own_key.trim();
        if !key.is_empty() {
            let service = own_key::Service::detect(key);
            let base_url =
                trim_url(&settings.own_base_url).unwrap_or_else(|| service.base_url().to_string());
            let model = if settings.own_model.trim().is_empty() {
                service.default_model().to_string()
            } else {
                settings.own_model.trim().to_string()
            };
            return Provider::Own {
                service,
                base_url,
                api_key: key.to_string(),
                model,
            };
        }
    }
    if let Ok(k) = std::env::var("PUBLIK_API_KEY") {
        let k = k.trim().to_string();
        if k.starts_with("pk_") {
            return Provider::Publik {
                base_url: resolve_base_url(""),
                api_key: k,
                model: MODEL_BALANCED.into(),
            };
        }
    }
    if settings.provider == settings::PROVIDER_PUBLIK || settings.provider.is_empty() {
        if let Some(c) = credential::read(app.slug) {
            return Provider::Publik {
                base_url: resolve_base_url(&c.base_url),
                api_key: c.key,
                model: MODEL_BALANCED.into(),
            };
        }
    }
    Provider::NotSetUp
}

/// Builder check, run by the BUILT app: start it with `PUBLIK_SELF_TEST=1`,
/// print this line, exit. It provisions for real; the builder who runs it is the consent.
pub fn self_test(app: &AppInfo) -> String {
    match provision::provision(app) {
        Ok(c) => format!(
            "publik self-test: ok {} {} (install {:.8})",
            app.slug, app.version, c.install_id
        ),
        Err(e) => format!("publik self-test: failed: {e}"),
    }
}

/// Dollars from micros, two decimals (`$0.05`).
pub fn dollars(micros: i64) -> String {
    let d = micros as f64 / 1_000_000.0;
    if d.abs() < 0.01 && micros != 0 {
        format!("${d:.4}")
    } else {
        format!("${d:.2}")
    }
}

/// First-run card, line 1. A new install is $0.00 until it is linked.
pub fn balance_line(micros: Option<i64>, claim_state: &str) -> String {
    let Some(m) = micros else {
        return String::new();
    };
    if m == 0 && claim_state != "claimed" {
        format!("{} · link this computer for $0.05 of free use", dollars(m))
    } else {
        format!("{} of publik balance", dollars(m))
    }
}

/// The only host a claim or top-up link may point at. Anything else is dropped.
pub fn publik_link(s: Option<&str>) -> Option<String> {
    let s = s?.trim();
    if s.starts_with("https://publikhq.com/") {
        Some(s.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_line_says_link_while_anonymous() {
        assert_eq!(
            balance_line(Some(0), "anonymous"),
            "$0.00 · link this computer for $0.05 of free use"
        );
        assert_eq!(
            balance_line(Some(1_250_000), "claimed"),
            "$1.25 of publik balance"
        );
        assert_eq!(balance_line(None, "claimed"), "");
    }

    #[test]
    fn only_publik_links_pass() {
        assert_eq!(
            publik_link(Some("https://publikhq.com/claim/AB12")),
            Some("https://publikhq.com/claim/AB12".into())
        );
        assert_eq!(publik_link(Some("https://evil.example/claim")), None);
        assert_eq!(publik_link(None), None);
    }

    #[test]
    fn dollars_keep_small_charges_visible() {
        assert_eq!(dollars(50_000), "$0.05");
        assert_eq!(dollars(1_200), "$0.0012");
        assert_eq!(dollars(0), "$0.00");
    }
}
