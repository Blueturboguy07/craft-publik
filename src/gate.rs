//! Sign in with publik: the gate every app on publik opens with (publikhq.com/sdk/publik.rs.md).
//!
//! `ensure_signed_in` runs at app start, before any screen the app owns. It
//!   1. loads or provisions this computer's publik key (`POST /api/v1/installs`);
//!   2. shows Sign in with publik until `GET /api/v1/wallet` says "claimed" (no other way past it);
//!   3. when the app uses AI: asks once how the app pays for AI and stores the answer
//!      (`crate::set_choice` changes it);
//!   4. starts usage counting: `app_opened` now, `active_session` every 30 minutes
//!      (`POST /api/v1/telemetry`, see `telemetry`).
//!
//! The words of the screens are the contract's, the same in every publik module. A host draws
//! them through `GateUi`: the egui panel (`ui::Gate`), a webview window (`html`), or the
//! `TerminalUi` of a command-line tool.

use std::io::{BufRead, Write};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{json, Value};

use crate::credential::{self, Credential};
use crate::settings::AiSettings;
use crate::{AppInfo, Choice};

/// The body of the screen shown when publik does not answer.
pub const UNREACHABLE: &str = "publik is not reachable. Check your connection.";

/// One screen of the gate: its words and its buttons as `(id, label)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    pub id: &'static str,
    pub title: &'static str,
    pub body: String,
    pub note: Option<&'static str>,
    pub buttons: Vec<(&'static str, &'static str)>,
}

/// How a host shows the gate. `show` draws the screen and returns the id of the button pressed.
/// Once `done` is set (the gate saw the sign-in or the plan), it closes the screen and returns
/// `None`. `open_url` opens the sign-in or plan page in the browser.
pub trait GateUi {
    fn show(&mut self, screen: &Screen, done: Arc<AtomicBool>) -> Option<&'static str>;
    fn open_url(&mut self, url: &str);
}

/// What the gate hands back once the person is signed in. Never the key: `crate::resolve`
/// reads it from the credential file.
#[derive(Clone, Debug)]
pub struct Passed {
    /// The wallet as `GET /wallet` last answered it (`claim_state`, `account`, `plan`,
    /// `balance_micros`).
    pub wallet: Value,
    /// The stored answer to the plan screen; `None` when the app does not use AI.
    pub choice: Option<Choice>,
}

impl Passed {
    /// The `@handle` of the signed-in account, when publik sent one.
    pub fn handle(&self) -> Option<&str> {
        self.wallet["account"]["handle"].as_str()
    }
}

static PASSED: AtomicBool = AtomicBool::new(false);
static ASK_AGAIN: AtomicBool = AtomicBool::new(false);

/// True once `ensure_signed_in` returned in this process. A host holds its own first-run
/// dialogs until then.
pub fn passed() -> bool {
    PASSED.load(Ordering::SeqCst)
}

/// A setting asked for the gate again (`crate::set_choice(app, None)`, a sign-out). The egui
/// `Gate` runs it again when it sees this; another host asks `take_ask_again`.
pub fn ask_again() {
    PASSED.store(false, Ordering::SeqCst);
    ASK_AGAIN.store(true, Ordering::SeqCst);
}

/// True once per `ask_again`.
pub fn take_ask_again() -> bool {
    ASK_AGAIN.swap(false, Ordering::SeqCst)
}

/// `GET /api/v1/wallet` with this computer's key: `(status, wallet or None, message)`.
/// `Err`: publik is not reachable.
pub fn read_wallet(
    app: &AppInfo,
    key: &str,
) -> Result<(u16, Option<Value>, Option<String>), String> {
    let res = crate::http::get(&crate::api_url(app.endpoints.wallet), Some(key), vec![], 15)
        .map_err(|_| UNREACHABLE.to_string())?;
    let mut body = res.json();
    if res.status != 200 || !body["claim_state"].is_string() {
        let message = body["error"]["message"].as_str().map(str::to_string);
        return Ok((res.status, None, message));
    }
    // Until publik sends `plan` (its next deploy), the plan is none.
    if !body["plan"].is_object() {
        body["plan"] = json!({ "kind": "none", "label": null });
    }
    Ok((res.status, Some(body), None))
}

/// The words of the gate. The same in every publik module.
pub fn gate_screen(
    id: &'static str,
    w: Option<&Value>,
    note: Option<&'static str>,
    message: Option<String>,
) -> Screen {
    if id == "unreachable" {
        return Screen {
            id,
            title: "Sign in with publik",
            body: message.unwrap_or_else(|| UNREACHABLE.to_string()),
            note,
            buttons: vec![("retry", "Retry")],
        };
    }
    if id == "sign_in" {
        return Screen {
            id,
            title: "Sign in with publik",
            note,
            buttons: vec![("sign_in", "Sign in with publik")],
            body: "This app uses your publik account. Click Sign in with publik, then sign in in your browser. This window continues when you are signed in.".into(),
        };
    }
    let w = w
        .cloned()
        .unwrap_or_else(|| json!({ "plan": { "kind": "none" } }));
    let who = match w["account"]["handle"].as_str() {
        Some(h) => format!("You are signed in as @{h}."),
        None => "You are signed in to publik.".to_string(),
    };
    if !has_plan(&w) {
        return Screen {
            id,
            title: "Choose how to pay for AI",
            note,
            body: format!("{who} This app uses AI. Get a publik plan, or use your own key."),
            buttons: vec![("get_plan", "Get a plan"), ("own_key", "Use my own key")],
        };
    }
    let what = if w["plan"]["kind"] == "plan" {
        format!(
            "Your plan: {}.",
            w["plan"]["label"].as_str().unwrap_or("publik")
        )
    } else {
        format!(
            "Your balance: ${:.2}.",
            w["balance_micros"].as_i64().unwrap_or(0) as f64 / 1_000_000.0
        )
    };
    Screen {
        id,
        title: "Connect your plan",
        note,
        body: format!("{who} {what} Connect it to use AI in this app, or use your own key."),
        buttons: vec![
            ("connect", "Connect your plan"),
            ("own_key", "Use my own key"),
        ],
    }
}

/// The install is signed in.
pub fn claimed(w: &Value) -> bool {
    w["claim_state"] == "claimed"
}

/// The account has a plan, a pack, or a balance.
pub fn has_plan(w: &Value) -> bool {
    w["plan"]["kind"].as_str().unwrap_or("none") != "none"
        || w["balance_micros"].as_i64().unwrap_or(0) > 0
}

/// Show a screen; with `until`, poll GET /wallet every 3 s for at most 10 minutes.
/// "done": the poll saw it; "timeout": it did not.
fn ask(
    app: &AppInfo,
    ui: &mut dyn GateUi,
    screen: &Screen,
    key: &str,
    until: Option<fn(&Value) -> bool>,
) -> &'static str {
    let done = Arc::new(AtomicBool::new(false));
    let polled: Arc<Mutex<Option<&'static str>>> = Arc::new(Mutex::new(None));
    if let Some(until) = until {
        let (done, polled, key, app) = (done.clone(), polled.clone(), key.to_string(), app.clone());
        std::thread::spawn(move || {
            let mut result = "timeout";
            'poll: for _ in 0..200 {
                // 200 x 3 s = 10 minutes
                for _ in 0..30 {
                    std::thread::sleep(Duration::from_millis(100));
                    if done.load(Ordering::SeqCst) {
                        return;
                    }
                }
                if let Ok((_, Some(w), _)) = read_wallet(&app, &key) {
                    if until(&w) {
                        result = "done";
                        break 'poll;
                    }
                }
            }
            *polled.lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
            done.store(true, Ordering::SeqCst);
        });
    }
    let action = ui.show(screen, done.clone());
    done.store(true, Ordering::SeqCst);
    let polled = *polled.lock().unwrap_or_else(PoisonError::into_inner);
    action.or(polled).unwrap_or("retry")
}

/// The gate (see the top of the file). Returns once the person is signed in and, when the app
/// uses AI, has answered the plan screen. There is no other way out: a host that cannot show
/// the gate keeps the app closed.
pub fn ensure_signed_in(app: &AppInfo, ui: &mut dyn GateUi) -> Passed {
    if std::env::var_os("PUBLIK_SELF_TEST").is_some() {
        println!("{}", self_test(app));
        std::process::exit(0);
    }
    let mut sign_note: Option<&'static str> = None;
    let mut sign_poll = true;
    let mut plan_note: Option<&'static str> = None;
    let mut plan_poll = false;
    loop {
        let env_key = std::env::var("PUBLIK_API_KEY")
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| k.starts_with("pk_"));
        let c: Result<Credential, String> = match &env_key {
            Some(k) => Ok(Credential {
                key: k.clone(),
                ..Default::default()
            }),
            None => match credential::read(app.slug) {
                Some(c) => Ok(c),
                None => crate::provision::provision(app),
            },
        };
        let c = match c {
            Ok(c) => c,
            Err(e) => {
                ask(
                    app,
                    ui,
                    &gate_screen("unreachable", None, None, Some(e)),
                    "",
                    None,
                );
                continue;
            }
        };
        let key = c.key.clone();
        let w = match read_wallet(app, &key) {
            Err(_) => {
                ask(
                    app,
                    ui,
                    &gate_screen("unreachable", None, None, None),
                    "",
                    None,
                );
                continue;
            }
            // A revoked key: provision again.
            Ok((401, _, _)) if env_key.is_none() => {
                credential::forget(app.slug);
                continue;
            }
            Ok((_, None, message)) => {
                ask(
                    app,
                    ui,
                    &gate_screen("unreachable", None, None, message),
                    "",
                    None,
                );
                continue;
            }
            Ok((_, Some(w), _)) => w,
        };
        if !claimed(&w) {
            let poll = if sign_poll {
                Some(claimed as fn(&Value) -> bool)
            } else {
                None
            };
            let a = ask(
                app,
                ui,
                &gate_screen("sign_in", Some(&w), sign_note, None),
                &key,
                poll,
            );
            if a == "sign_in" {
                let url = w["claim_url"]
                    .as_str()
                    .and_then(|u| crate::publik_link(Some(u)))
                    .or_else(|| crate::publik_link(Some(&c.claim_url)))
                    .unwrap_or_else(|| format!("{}/wallet", crate::origin()));
                ui.open_url(&url);
                sign_note = Some("Waiting for you to sign in in your browser.");
                sign_poll = true;
            }
            if a == "timeout" {
                sign_note =
                    Some("No sign-in after 10 minutes. Click Sign in with publik to try again.");
                sign_poll = false;
            }
            continue;
        }
        let settings = AiSettings::load(app.settings_path().as_deref());
        let mut choice = if app.uses_ai {
            crate::choice(app, &settings)
        } else {
            None
        };
        if app.uses_ai && choice.is_none() {
            if has_plan(&w) {
                // The plan is there: Connect your plan.
                plan_note = None;
                plan_poll = false;
            }
            let poll = if plan_poll {
                Some(has_plan as fn(&Value) -> bool)
            } else {
                None
            };
            let a = ask(
                app,
                ui,
                &gate_screen("choose", Some(&w), plan_note, None),
                &key,
                poll,
            );
            plan_note = None;
            plan_poll = false;
            if a == "get_plan" {
                let url = w["top_up_url"]
                    .as_str()
                    .and_then(|u| crate::publik_link(Some(u)))
                    .unwrap_or_else(|| format!("{}/wallet", crate::origin()));
                ui.open_url(&url);
                plan_note = Some("Waiting for your plan. Finish in your browser.");
                plan_poll = true;
            }
            if a == "timeout" {
                plan_note = Some("No plan after 10 minutes. Click Get a plan to try again.");
            }
            if a != "connect" && a != "own_key" {
                // "done": the plan is there; the next screen is Connect your plan.
                continue;
            }
            choice = Some(if a == "connect" {
                Choice::Publik
            } else {
                Choice::OwnKey
            });
            crate::set_choice(app, choice);
        }
        PASSED.store(true, Ordering::SeqCst);
        crate::telemetry::start(app);
        return Passed { wallet: w, choice };
    }
}

/// Builder check, run by the BUILT app: `PUBLIK_SELF_TEST=1` makes `ensure_signed_in` print
/// this line and exit. It is `ok` only after a real `GET /wallet` answered.
pub fn self_test(app: &AppInfo) -> String {
    let result = crate::provision::provision(app)
        .and_then(|c| read_wallet(app, &c.key))
        .and_then(|(code, w, message)| match w {
            Some(_) => Ok(()),
            None => Err(message.unwrap_or_else(|| format!("GET /wallet answered {code}"))),
        });
    match result {
        Ok(()) => format!("publik self-test: ok {} {}", app.slug, app.version),
        Err(e) => format!("publik self-test: failed: {e}"),
    }
}

/// Opens a URL with the system's default browser. A host without its own opener uses it.
pub fn open_in_browser(url: &str) {
    let _ = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).spawn()
    } else if cfg!(windows) {
        Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn()
    } else {
        Command::new("xdg-open").arg(url).spawn()
    };
}

/// A terminal UI for a command-line app. A desktop app passes its own `GateUi`.
pub struct TerminalUi;

impl GateUi for TerminalUi {
    fn show(&mut self, s: &Screen, done: Arc<AtomicBool>) -> Option<&'static str> {
        println!(
            "\n{}\n{}{}",
            s.title,
            s.body,
            s.note.map(|n| format!("\n{n}")).unwrap_or_default()
        );
        for (i, (_, label)) in s.buttons.iter().enumerate() {
            println!("  {}. {label}", i + 1);
        }
        let answered = Arc::new(AtomicBool::new(false));
        let (d, a) = (done.clone(), answered.clone());
        std::thread::spawn(move || {
            while !d.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(200));
            }
            if !a.load(Ordering::SeqCst) {
                println!("\nDone. Press Enter to continue.");
            }
        });
        loop {
            print!(
                "{}",
                if s.buttons.len() > 1 {
                    "Type a number and press Enter: "
                } else {
                    "Press Enter: "
                }
            );
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
                while !done.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(200));
                }
                return None;
            }
            if done.load(Ordering::SeqCst) {
                return None;
            }
            let n: usize = line.trim().parse().unwrap_or(1);
            if let Some((action, _)) = s.buttons.get(n.wrapping_sub(1)) {
                answered.store(true, Ordering::SeqCst);
                return Some(*action);
            }
        }
    }

    fn open_url(&mut self, url: &str) {
        println!("Open this link if your browser does not open: {url}");
        open_in_browser(url);
    }
}

/// The screen as one HTML page for a webview host (a Tauri window). Each button navigates to
/// `press_base` + its id (for example `publik-gate://localhost/press/sign_in`), which the host
/// catches and answers from `show`.
pub fn html(screen: &Screen, press_base: &str) -> String {
    let mut buttons = String::new();
    for (i, (id, label)) in screen.buttons.iter().enumerate() {
        let class = if i == 0 { "primary" } else { "secondary" };
        buttons.push_str(&format!(
            "<a class=\"button {class}\" href=\"{}{}\">{}</a>",
            escape_html(press_base),
            escape_html(id),
            escape_html(label)
        ));
    }
    let note = screen
        .note
        .map(|n| format!("<p class=\"note\">{}</p>", escape_html(n)))
        .unwrap_or_default();
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title><style>\
html,body{{margin:0;height:100%;font:15px/1.5 -apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;background:#141416;color:#f2f2f2}}\
main{{box-sizing:border-box;min-height:100%;display:flex;flex-direction:column;justify-content:center;padding:32px 36px;max-width:460px;margin:0 auto}}\
h1{{font-size:22px;margin:0 0 12px}}p{{margin:0 0 12px}}.note{{color:#b0b0b6}}\
.buttons{{display:flex;gap:10px;flex-wrap:wrap;margin-top:8px}}\
.button{{display:inline-block;padding:9px 16px;border-radius:8px;text-decoration:none;font-weight:600}}\
.primary{{background:#4f7cff;color:#fff}}.secondary{{background:#2a2a2e;color:#f2f2f2}}\
</style></head><body><main><h1>{title}</h1><p>{body}</p>{note}<div class=\"buttons\">{buttons}</div></main></body></html>",
        title = escape_html(screen.title),
        body = escape_html(&screen.body),
    )
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_screens_carry_the_contracts_words() {
        let s = gate_screen("sign_in", None, None, None);
        assert_eq!(s.title, "Sign in with publik");
        assert_eq!(s.buttons, vec![("sign_in", "Sign in with publik")]);
        assert!(s.body.starts_with("This app uses your publik account."));

        let s = gate_screen("unreachable", None, None, None);
        assert_eq!(s.body, UNREACHABLE);
        assert_eq!(s.buttons, vec![("retry", "Retry")]);

        let empty = json!({ "claim_state": "claimed", "account": { "handle": "mann" }, "plan": { "kind": "none" }, "balance_micros": 0 });
        let s = gate_screen("choose", Some(&empty), None, None);
        assert_eq!(s.title, "Choose how to pay for AI");
        assert_eq!(
            s.body,
            "You are signed in as @mann. This app uses AI. Get a publik plan, or use your own key."
        );
        assert_eq!(
            s.buttons,
            vec![("get_plan", "Get a plan"), ("own_key", "Use my own key")]
        );

        let plan = json!({ "claim_state": "claimed", "account": { "handle": "mann" }, "plan": { "kind": "plan", "label": "Plus" }, "balance_micros": 0 });
        let s = gate_screen("choose", Some(&plan), None, None);
        assert_eq!(s.title, "Connect your plan");
        assert_eq!(s.body, "You are signed in as @mann. Your plan: Plus. Connect it to use AI in this app, or use your own key.");
        assert_eq!(
            s.buttons,
            vec![
                ("connect", "Connect your plan"),
                ("own_key", "Use my own key")
            ]
        );

        let balance = json!({ "claim_state": "claimed", "plan": { "kind": "none" }, "balance_micros": 50_000 });
        let s = gate_screen("choose", Some(&balance), None, None);
        assert_eq!(s.body, "You are signed in to publik. Your balance: $0.05. Connect it to use AI in this app, or use your own key.");
        assert!(has_plan(&balance) && !has_plan(&empty) && claimed(&plan));
    }

    #[test]
    fn the_html_page_escapes_and_links_every_button() {
        let s = gate_screen(
            "sign_in",
            None,
            Some("Waiting for you to sign in in your browser."),
            None,
        );
        let page = html(&s, "publik-gate://localhost/press/");
        assert!(page.contains("<h1>Sign in with publik</h1>"));
        assert!(page.contains("href=\"publik-gate://localhost/press/sign_in\""));
        assert!(page.contains("Waiting for you to sign in in your browser."));
        assert_eq!(escape_html("<a & \"b\">"), "&lt;a &amp; &quot;b&quot;&gt;");
    }
}
