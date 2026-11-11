//! publik for a Craft app: Sign in with publik, usage counting, and publik API as one AI
//! provider option next to the person's own key.
//!
//! Every app on publik opens with Sign in with publik. This crate is the publik module
//! (publikhq.com/sdk/publik.rs.md) for the Craft apps, shared instead of copied: the gate
//! (`gate::ensure_signed_in`), usage counting (`telemetry`), provisioning (`POST /installs`
//! with the app's public token), the per-app credential file publik writes on this computer,
//! the OpenAI-compatible calls (chat, images, transcription, speech), the person's own-key
//! path, and (behind the `egui` feature) the gate panel and the AI settings every Craft app
//! shows. Each app names itself in its own source (`AppInfo`: its `APP_SLUG`, version,
//! `USES_AI`, token and endpoints) and runs the gate at start-up, before any screen it owns.
//!
//! Resolution order: the person's stored answer to the plan screen decides. First "Use my own
//! key"; then `PUBLIK_API_KEY` / `PUBLIK_API_BASE_URL` in the environment (development); then
//! "Connect your plan" with this app's credential file; else nothing usable yet (the tools ask
//! for a key).
//!
//! Copy rule (publik contract): "publik API", dollars, never tokens, never "credits" as a unit,
//! never the model vendor's name next to a price.
//!
//! HTTP goes through the system `curl` (macOS, Windows 10+ and Linux ship it), so no TLS crate
//! joins the app. A key only ever rides a curl config on stdin, never argv.

pub mod client;
pub mod credential;
pub mod gate;
pub mod http;
pub mod own_key;
pub mod provision;
pub mod settings;
pub mod telemetry;
#[cfg(feature = "egui")]
pub mod ui;

use std::path::PathBuf;

pub use client::{Fail, Reply};
pub use credential::Credential;
pub use gate::{GateUi, Passed, Screen};
pub use settings::AiSettings;

/// publik's origin. `PUBLIK_ORIGIN` overrides it (tests only).
pub const DEFAULT_ORIGIN: &str = "https://publikhq.com";
pub const DEFAULT_BASE_URL: &str = "https://publikhq.com/api/v1";
pub const DASHBOARD_URL: &str = "https://publikhq.com/dashboard/api";
/// Tier aliases. Ask for a tier, never a vendor model name.
pub const MODEL_FAST: &str = "publik-fast";
pub const MODEL_BALANCED: &str = "publik-balanced";
pub const MODEL_SMART: &str = "publik-smart";
/// The image, transcription and speech lines have no tier alias; these are the
/// names the gateway's price sheet carries for them.
pub const IMAGE_MODEL: &str = "gpt-image-1";
/// The second image line: Nano Banana (Google's image model) behind publik's alias on the
/// Gemini line. publik API only; an own key has no Gemini line in this crate.
pub const NANO_BANANA_MODEL: &str = "publik-image";
pub const TRANSCRIBE_MODEL: &str = "whisper-1";
pub const SPEECH_MODEL: &str = "tts-1";
pub const DISCLOSURE_VERSION: u32 = 1;

/// The two-sentence disclosure of publik API, for an app's settings page. Never promises a starter.
pub const DISCLOSURE: &str = "This app can use publik API for its AI features: no key to paste, priced per use in dollars from a publik balance that starts at $0.00. Linking this computer to a publik account gives $0.05 of free use, once. Your prompts go through publik's servers to the model; you can switch to your own key at any time.";
pub const DISCLOSURE_DATA: &str = "What you send to an AI feature goes through publik's servers to a shared model account. publik never trains on it and does not store it. Everything else in this app stays on this computer.";
/// The one-sentence justification shown wherever money is mentioned, when the
/// provisioning reply did not carry its own.
pub const WHY_IT_COSTS: &str = "The model's provider charges per use. publik charges a fixed, published price that sits above what the model costs publik; the difference keeps publik running and pays the developer who built this app. Nothing is charged behind your back, and every call is visible on your publik dashboard.";
pub const CTA_SIGN_IN: &str = "Sign in with publik";
pub const CTA_ADD_PLAN: &str = "Add a plan or pack";
pub const CTA_LATER: &str = "Later";

/// The publik paths and event names the module calls. The app writes them in its own source,
/// next to its `APP_SLUG` (publik's scan reads the module's endpoints and its slug from one
/// file of the app); `Endpoints::DEFAULT` is the contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoints {
    /// `POST`: this computer's key.
    pub installs: &'static str,
    /// `GET`: the claim state, account, plan and balance.
    pub wallet: &'static str,
    /// `POST`: one usage event.
    pub telemetry: &'static str,
    /// The event sent once the gate passed.
    pub opened: &'static str,
    /// The event sent every 30 minutes after it.
    pub active: &'static str,
}

impl Endpoints {
    pub const DEFAULT: Endpoints = Endpoints {
        installs: "/api/v1/installs",
        wallet: "/api/v1/wallet",
        telemetry: "/api/v1/telemetry",
        opened: "app_opened",
        active: "active_session",
    };
}

impl Default for Endpoints {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The person's answer to the plan screen of the gate, kept in the credential file (`choice`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// "Connect your plan": the install key pays, from the person's publik plan or balance.
    Publik,
    /// "Use my own key": the person's own key at its own service, exactly as before.
    OwnKey,
}

impl Choice {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Publik => "publik",
            Self::OwnKey => "own_key",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "publik" => Some(Self::Publik),
            "own_key" => Some(Self::OwnKey),
            _ => None,
        }
    }
}

/// What the host app tells this crate about itself. The app writes these in its own source.
#[derive(Clone, Debug)]
pub struct AppInfo {
    /// publik listing slug (`gridcraft`). Also the credential file's name.
    pub slug: &'static str,
    /// Display name (`GridCraft`).
    pub name: &'static str,
    /// The release version, as the listing shows it (`0.9.0-publik.4`).
    pub version: &'static str,
    /// The app's public `pat_` token, compiled in from `publik-app-token.txt`.
    /// Empty means this build has no token: the gate says so and cannot pass.
    pub app_token: &'static str,
    /// False: the app calls no model. It still signs in and counts users, and it skips the
    /// plan screen.
    pub uses_ai: bool,
    /// The paths and events the module calls (`Endpoints::DEFAULT`).
    pub endpoints: Endpoints,
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
    /// Nothing usable yet: no plan is connected and no own key is pasted.
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

/// publik's origin: `PUBLIK_ORIGIN` (tests only), else publikhq.com.
pub fn origin() -> String {
    std::env::var("PUBLIK_ORIGIN")
        .ok()
        .and_then(|s| trim_url(&s))
        .unwrap_or_else(|| DEFAULT_ORIGIN.to_string())
}

/// `origin()` + a path such as `Endpoints::DEFAULT.wallet`.
pub fn api_url(path: &str) -> String {
    format!("{}{path}", origin())
}

/// The gateway's base URL at the origin in force.
pub fn default_base_url() -> String {
    format!("{}/api/v1", origin())
}

/// `PUBLIK_API_BASE_URL` (dev) > the provisioning reply's `base_url` > the default.
pub fn resolve_base_url(from_credential: &str) -> String {
    if let Some(u) = std::env::var("PUBLIK_API_BASE_URL")
        .ok()
        .and_then(|s| trim_url(&s))
    {
        return u;
    }
    trim_url(from_credential).unwrap_or_else(default_base_url)
}

fn trim_url(s: &str) -> Option<String> {
    let t = s.trim().trim_end_matches('/');
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// The stored answer to the plan screen: the credential file's `choice`, or, for a computer set
/// up before the gate, the provider `ai.json` named (`own` or `publik`). None: not answered yet.
pub fn choice(app: &AppInfo, settings: &AiSettings) -> Option<Choice> {
    if let Some(c) = credential::read_choice(app.slug) {
        return Some(c);
    }
    match settings.provider.as_str() {
        settings::PROVIDER_OWN => Some(Choice::OwnKey),
        settings::PROVIDER_PUBLIK => Some(Choice::Publik),
        _ => None,
    }
}

/// Store the answer to the plan screen: `Some(Choice::Publik)`, `Some(Choice::OwnKey)`, or
/// `None` to ask again at the next gate (a setting calls it; see `gate::ask_again`).
pub fn set_choice(app: &AppInfo, choice: Option<Choice>) {
    let _ = credential::set_choice(app.slug, choice.map(Choice::as_str));
}

/// The provider in force, by the resolution order above. Pure: reads files, never the network.
pub fn resolve(app: &AppInfo, settings: &AiSettings) -> Provider {
    let choice = choice(app, settings);
    if choice == Some(Choice::OwnKey) {
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
        return Provider::NotSetUp;
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
    if choice == Some(Choice::Publik) {
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

/// Builder check, run by the BUILT app: `PUBLIK_SELF_TEST=1` makes the gate print this line and
/// exit. It provisions for real and reads the wallet; the builder who runs it is the consent.
pub fn self_test(app: &AppInfo) -> String {
    gate::self_test(app)
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

/// The balance line of the settings page. A new install is $0.00 until it is linked.
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

/// The only host a claim or top-up link may point at (publikhq.com, or `PUBLIK_ORIGIN` in
/// tests). Anything else is dropped.
pub fn publik_link(s: Option<&str>) -> Option<String> {
    let s = s?.trim();
    if s.starts_with("https://publikhq.com/") || s.starts_with(&format!("{}/", origin())) {
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

    #[test]
    fn choices_round_trip_as_the_credential_stores_them() {
        assert_eq!(Choice::parse("publik"), Some(Choice::Publik));
        assert_eq!(Choice::parse(" own_key "), Some(Choice::OwnKey));
        assert_eq!(Choice::parse("later"), None);
        assert_eq!(Choice::Publik.as_str(), "publik");
        assert_eq!(Choice::OwnKey.as_str(), "own_key");
    }

    #[test]
    fn the_default_endpoints_are_the_contract() {
        let e = Endpoints::default();
        assert_eq!(e.installs, "/api/v1/installs");
        assert_eq!(e.wallet, "/api/v1/wallet");
        assert_eq!(e.telemetry, "/api/v1/telemetry");
        assert_eq!(e.opened, "app_opened");
        assert_eq!(e.active, "active_session");
        assert_eq!(api_url(e.wallet), format!("{}/api/v1/wallet", origin()));
    }

    #[test]
    fn an_own_key_choice_without_a_key_is_not_set_up() {
        let app = AppInfo {
            slug: "craft-publik-unit-test-no-credential",
            name: "Test",
            version: "0.0.0",
            app_token: "",
            uses_ai: true,
            endpoints: Endpoints::DEFAULT,
            dialects: &["chat_completions"],
            config_dir: None,
        };
        let empty = AiSettings {
            provider: settings::PROVIDER_OWN.into(),
            ..Default::default()
        };
        assert_eq!(choice(&app, &empty), Some(Choice::OwnKey));
        assert_eq!(resolve(&app, &empty), Provider::NotSetUp);
        let with_key = AiSettings {
            provider: settings::PROVIDER_OWN.into(),
            own_key: "sk-proj-unit".into(),
            ..Default::default()
        };
        assert!(matches!(resolve(&app, &with_key), Provider::Own { .. }));
        assert_eq!(choice(&app, &AiSettings::default()), None);
    }
}
