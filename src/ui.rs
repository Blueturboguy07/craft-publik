//! The egui setup flow every Craft app shows on first launch and in its
//! preferences: two cards (publik API, your own key), the two-sentence
//! disclosure, provisioning, the first-run card (balance line, cost sentence,
//! Sign in with publik), and the own-key form. The host renders it inside its
//! own window; this module never opens windows and never opens URLs itself.

use std::sync::mpsc::{channel, Receiver};

use egui::{RichText, Ui};

use crate::client::{Fail, Reply};
use crate::credential::{self, Credential};
use crate::own_key::{self, Service};
use crate::settings::{AiSettings, PROVIDER_OWN, PROVIDER_PUBLIK};
use crate::{
    AppInfo, Provider, CTA_ADD_PLAN, CTA_LATER, CTA_SIGN_IN, DISCLOSURE, DISCLOSURE_DATA,
    DISCLOSURE_VERSION, WHY_IT_COSTS,
};

/// Which screen of the setup flow is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Pick,
    Disclosure,
    Provisioning,
    Card,
    OwnKey,
    Probing,
}

/// What the host should do after drawing a frame of the flow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    /// Open this URL in the system browser (always a publikhq.com link, or the dashboard).
    OpenUrl(String),
    /// The flow finished or was dismissed: close the host window.
    Close,
}

/// A notice a feature left behind (a 402, a retired key): one message, at most one link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub message: String,
    pub link_label: String,
    pub link_url: Option<String>,
}

enum Event {
    Provisioned(Result<Credential, String>),
    Probed(Result<Vec<String>, String>),
}

pub struct AiState {
    pub app: AppInfo,
    pub settings: AiSettings,
    pub credential: Option<Credential>,
    pub step: Step,
    pub error: Option<String>,
    pub notice: Option<Notice>,
    /// The last call's "Charged $x · $y left" line, for status bars.
    pub last_charge: String,
    pub key_draft: String,
    pub base_url_draft: String,
    pub model_draft: String,
    pub show_key: bool,
    pub advanced: bool,
    probed: Option<Vec<String>>,
    rx: Option<Receiver<Event>>,
}

impl AiState {
    pub fn new(app: AppInfo) -> Self {
        let settings = AiSettings::load(app.settings_path().as_deref());
        let credential = credential::read(app.slug);
        let mut s = AiState {
            app,
            settings,
            credential,
            step: Step::Pick,
            error: None,
            notice: None,
            last_charge: String::new(),
            key_draft: String::new(),
            base_url_draft: String::new(),
            model_draft: String::new(),
            show_key: false,
            advanced: false,
            probed: None,
            rx: None,
        };
        s.key_draft = s.settings.own_key.clone();
        s.base_url_draft = s.settings.own_base_url.clone();
        s.model_draft = s.settings.own_model.clone();
        s
    }

    pub fn provider(&self) -> Provider {
        crate::resolve(&self.app, &self.settings)
    }

    /// Show the setup flow on this launch: nothing chosen yet and the flow never ran to an end.
    pub fn wants_first_run(&self) -> bool {
        !self.settings.setup_done && !self.provider().is_set_up()
    }

    /// A publik install that was set up but never shown its first-run card.
    pub fn owes_first_card(&self) -> bool {
        self.provider().is_publik() && self.credential.is_some() && !self.settings.first_card_shown
    }

    pub fn begin(&mut self) {
        self.error = None;
        self.step = if self.owes_first_card() {
            Step::Card
        } else {
            Step::Pick
        };
    }

    fn save(&mut self) {
        if let Err(e) = self.settings.save(self.app.settings_path().as_deref()) {
            self.error = Some(format!("Could not save AI settings: {e}"));
        }
    }

    /// Drain finished background work. Call once per frame before drawing.
    pub fn poll(&mut self) {
        let Some(rx) = self.rx.take() else { return };
        let mut done = false;
        loop {
            let ev = match rx.try_recv() {
                Ok(ev) => ev,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    done = true;
                    self.error =
                        Some("The background task stopped unexpectedly. Try again.".into());
                    if matches!(self.step, Step::Provisioning | Step::Probing) {
                        self.step = Step::Pick;
                    }
                    break;
                }
            };
            done = true;
            match ev {
                Event::Provisioned(Ok(c)) => {
                    self.credential = Some(c);
                    self.settings.provider = PROVIDER_PUBLIK.into();
                    self.settings.disclosure_accepted = DISCLOSURE_VERSION;
                    self.settings.setup_done = true;
                    self.save();
                    self.error = None;
                    self.step = Step::Card;
                }
                Event::Provisioned(Err(e)) => {
                    self.error = Some(e);
                    self.step = Step::Pick;
                }
                Event::Probed(Ok(ids)) => {
                    let service = Service::detect(&self.key_draft);
                    if self.model_draft.trim().is_empty() {
                        self.model_draft = own_key::pick_model(service, &ids);
                    }
                    self.probed = Some(ids);
                    self.settings.provider = PROVIDER_OWN.into();
                    self.settings.own_key = self.key_draft.trim().to_string();
                    self.settings.own_base_url = self.base_url_draft.trim().to_string();
                    self.settings.own_model = self.model_draft.trim().to_string();
                    self.settings.setup_done = true;
                    self.save();
                    self.error = None;
                    self.step = Step::OwnKey;
                }
                Event::Probed(Err(e)) => {
                    self.error = Some(e);
                    self.step = Step::OwnKey;
                }
            }
        }
        if !done {
            self.rx = Some(rx);
        }
    }

    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }

    fn start_provision(&mut self, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let app = self.app.clone();
        let ctx = ctx.clone();
        self.rx = Some(rx);
        self.step = Step::Provisioning;
        std::thread::spawn(move || {
            let r = crate::provision::provision(&app);
            let _ = tx.send(Event::Provisioned(r));
            ctx.request_repaint();
        });
    }

    fn start_probe(&mut self, ctx: &egui::Context) {
        let key = self.key_draft.trim().to_string();
        let service = Service::detect(&key);
        let base = if self.base_url_draft.trim().is_empty() {
            service.base_url().to_string()
        } else {
            self.base_url_draft.trim().to_string()
        };
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        self.rx = Some(rx);
        self.step = Step::Probing;
        std::thread::spawn(move || {
            let r = own_key::probe(service, &base, &key);
            let _ = tx.send(Event::Probed(r));
            ctx.request_repaint();
        });
    }

    /// Record a failed call: a 402 or a retired key becomes the notice; the person keeps working.
    pub fn note_fail(&mut self, f: &Fail) {
        if f.status == 402 || f.reprovision || f.status == 401 {
            self.notice = Some(Notice {
                message: f.message.clone(),
                link_label: f.link_label.clone(),
                link_url: f.link_url.clone(),
            });
        }
        if f.reprovision {
            credential::forget(self.app.slug);
            self.credential = None;
        }
    }

    /// Record a successful call: live balance and claim state, and the charge line.
    pub fn note_reply(&mut self, r: &Reply) {
        self.last_charge = r.charge_line();
        if let Some(c) = &mut self.credential {
            if let Some(b) = r.balance_micros {
                c.balance_micros = Some(b);
            }
            if let Some(s) = &r.claim_state {
                c.claim_state = s.clone();
            }
        }
        self.notice = None;
    }

    pub fn dismiss_notice(&mut self) {
        self.notice = None;
    }

    pub fn status_line(&self) -> String {
        let p = self.provider();
        match (&p, &self.credential) {
            (Provider::Publik { .. }, Some(c)) => format!(
                "publik API · {}",
                crate::balance_line(c.balance_micros, &c.claim_state)
            ),
            _ => p.label(),
        }
    }

    fn primary(ui: &mut Ui, label: &str) -> egui::Response {
        let fill = ui.visuals().selection.bg_fill;
        let text = RichText::new(label)
            .strong()
            .color(ui.visuals().selection.stroke.color);
        ui.add(
            egui::Button::new(text)
                .fill(fill)
                .min_size(egui::vec2(0.0, 26.0)),
        )
    }

    // ---- the setup flow -------------------------------------------------------------------------

    /// Draw the current step. The host wraps this in its own dialog window.
    pub fn setup_ui(&mut self, ui: &mut Ui) -> Action {
        self.poll();
        let ctx = ui.ctx().clone();
        ui.set_max_width(460.0);
        match self.step {
            Step::Pick => self.pick_ui(ui),
            Step::Disclosure => self.disclosure_ui(ui, &ctx),
            Step::Provisioning => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Setting up publik API on this computer…");
                });
                Action::None
            }
            Step::Card => self.card_ui(ui),
            Step::OwnKey | Step::Probing => self.own_key_ui(ui, &ctx),
        }
    }

    fn pick_ui(&mut self, ui: &mut Ui) -> Action {
        let mut action = Action::None;
        ui.label(format!("{} can use AI for a few of its tools. Pick how, or come back to this later from Options.", self.app.name));
        ui.add_space(8.0);
        let current = self.provider();
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("publik API").strong());
                if current.is_publik() {
                    ui.label(RichText::new("· in use").weak());
                }
            });
            ui.label("No key to paste. Priced per use in dollars from a publik balance; link this computer once for $0.05 of free use.");
            ui.add_space(4.0);
            if self.app.can_provision() {
                if Self::primary(ui, "Use publik API").clicked() {
                    self.error = None;
                    self.step = Step::Disclosure;
                }
            } else {
                ui.label(RichText::new("This build has no publik API app token.").weak());
            }
        });
        ui.add_space(6.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Your own key").strong());
                if matches!(current, Provider::Own { .. }) {
                    ui.label(RichText::new("· in use").weak());
                }
            });
            ui.label("Paste a key from OpenAI, OpenRouter, Groq, Google or Anthropic. Billed by that service, not by publik.");
            ui.add_space(4.0);
            if ui.button("Use my own key").clicked() {
                self.error = None;
                self.step = Step::OwnKey;
            }
        });
        if let Some(e) = &self.error {
            ui.add_space(6.0);
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(CTA_LATER).clicked() {
                self.settings.setup_done = true;
                self.save();
                action = Action::Close;
            }
        });
        action
    }

    fn disclosure_ui(&mut self, ui: &mut Ui, ctx: &egui::Context) -> Action {
        ui.label(RichText::new("Before publik API is set up").strong());
        ui.add_space(4.0);
        ui.label(DISCLOSURE);
        ui.add_space(4.0);
        ui.label(DISCLOSURE_DATA);
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label("The terms: ");
            ui.hyperlink_to("publikhq.com/terms", "https://publikhq.com/terms#api");
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if Self::primary(ui, "Continue").clicked() {
                self.start_provision(ctx);
            }
            if ui.button("Back").clicked() {
                self.step = Step::Pick;
            }
        });
        Action::None
    }

    fn card_ui(&mut self, ui: &mut Ui) -> Action {
        let mut action = Action::None;
        let Some(c) = self.credential.clone() else {
            self.step = Step::Pick;
            return Action::None;
        };
        ui.label(RichText::new("publik API is set up on this computer").strong());
        ui.add_space(4.0);
        ui.label(
            RichText::new(crate::balance_line(
                c.balance_micros.or(Some(0)),
                &c.claim_state,
            ))
            .strong(),
        );
        ui.add_space(4.0);
        ui.label(if c.cost_sentence.is_empty() {
            WHY_IT_COSTS
        } else {
            c.cost_sentence.as_str()
        });
        ui.add_space(8.0);
        let claimed = c.claim_state == "claimed";
        ui.horizontal(|ui| {
            if claimed {
                if !c.add_credit_url.is_empty() && Self::primary(ui, CTA_ADD_PLAN).clicked() {
                    action = Action::OpenUrl(c.add_credit_url.clone());
                }
            } else if !c.claim_url.is_empty() && Self::primary(ui, CTA_SIGN_IN).clicked() {
                action = Action::OpenUrl(c.claim_url.clone());
            }
            if ui
                .button(if claimed { "Done" } else { CTA_LATER })
                .clicked()
            {
                action = Action::Close;
            }
        });
        if !self.settings.first_card_shown {
            self.settings.first_card_shown = true;
            self.save();
        }
        action
    }

    fn own_key_ui(&mut self, ui: &mut Ui, ctx: &egui::Context) -> Action {
        let mut action = Action::None;
        let probing = self.step == Step::Probing;
        ui.label(RichText::new("Your own key").strong());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_enabled(
                !probing,
                egui::TextEdit::singleline(&mut self.key_draft)
                    .password(!self.show_key)
                    .desired_width(300.0)
                    .hint_text("sk-…, sk-or-…, gsk_…, AIza…, sk-ant-…"),
            );
            ui.checkbox(&mut self.show_key, "Show");
        });
        let service = Service::detect(&self.key_draft);
        if !self.key_draft.trim().is_empty() {
            ui.label(RichText::new(format!("Looks like a {} key.", service.name())).weak());
        }
        ui.add_space(2.0);
        ui.collapsing("Advanced", |ui| {
            egui::Grid::new("craft_publik_own_advanced")
                .num_columns(2)
                .spacing(egui::vec2(12.0, 6.0))
                .show(ui, |ui| {
                    ui.label("Base URL");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.base_url_draft)
                            .desired_width(300.0)
                            .hint_text(service.base_url()),
                    );
                    ui.end_row();
                    ui.label("Model");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.model_draft)
                            .desired_width(300.0)
                            .hint_text(service.default_model()),
                    );
                    ui.end_row();
                });
        });
        if let Some(ids) = &self.probed {
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!(
                    "The key works: {} models available. Using {}.",
                    ids.len(),
                    self.settings.own_model
                ))
                .color(ui.visuals().hyperlink_color),
            );
        }
        if let Some(e) = &self.error {
            ui.add_space(4.0);
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if probing {
                ui.spinner();
                ui.label("Checking the key…");
            } else {
                let can = !self.key_draft.trim().is_empty()
                    && (service != Service::Other || !self.base_url_draft.trim().is_empty());
                if ui
                    .add_enabled(can, egui::Button::new("Check and use this key"))
                    .clicked()
                {
                    self.error = None;
                    self.probed = None;
                    self.start_probe(ctx);
                }
                if self.probed.is_some() && ui.button("Done").clicked() {
                    action = Action::Close;
                }
                if ui.button("Back").clicked() {
                    self.step = Step::Pick;
                }
            }
        });
        action
    }

    // ---- the preferences section ----------------------------------------------------------------

    /// The AI section of the app's Options page. `open_setup` is set when the person wants the flow.
    pub fn options_ui(&mut self, ui: &mut Ui, open_setup: &mut bool) -> Action {
        self.poll();
        let mut action = Action::None;
        let p = self.provider();
        let cred = self.credential.clone();
        ui.label(self.status_line());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .button(if p.is_set_up() {
                    "Change…"
                } else {
                    "Set up AI…"
                })
                .clicked()
            {
                self.begin();
                *open_setup = true;
            }
            match (&p, &cred) {
                (Provider::Publik { .. }, Some(c)) => {
                    if c.claim_state == "claimed" {
                        if !c.add_credit_url.is_empty() && ui.button(CTA_ADD_PLAN).clicked() {
                            action = Action::OpenUrl(c.add_credit_url.clone());
                        }
                    } else if !c.claim_url.is_empty() && ui.button(CTA_SIGN_IN).clicked() {
                        action = Action::OpenUrl(c.claim_url.clone());
                    }
                    if ui.button("Dashboard").clicked() {
                        action = Action::OpenUrl(crate::DASHBOARD_URL.into());
                    }
                    if ui.button("Forget this computer's key").clicked() {
                        credential::forget(self.app.slug);
                        self.credential = None;
                        self.settings.provider = String::new();
                        self.settings.first_card_shown = false;
                        self.save();
                    }
                }
                (Provider::Own { .. }, _) => {
                    let remove = ui.button("Remove key").clicked();
                    if remove {
                        self.settings.own_key = String::new();
                        self.settings.provider = String::new();
                        self.key_draft = String::new();
                        self.probed = None;
                        self.save();
                    }
                }
                _ => {}
            }
        });
        if let Some(c) = &self.credential {
            if p.is_publik() {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(if c.cost_sentence.is_empty() {
                        WHY_IT_COSTS
                    } else {
                        c.cost_sentence.as_str()
                    })
                    .weak(),
                );
            }
        }
        if let Some(n) = self.notice.clone() {
            ui.add_space(6.0);
            ui.colored_label(ui.visuals().warn_fg_color, &n.message);
            if let Some(u) = &n.link_url {
                if ui.button(&n.link_label).clicked() {
                    action = Action::OpenUrl(u.clone());
                }
            }
        }
        action
    }

    /// A small bar a feature shows under its result: the notice (with its one link) or the charge.
    pub fn result_bar(&mut self, ui: &mut Ui) -> Action {
        let mut action = Action::None;
        if let Some(n) = self.notice.clone() {
            ui.colored_label(ui.visuals().warn_fg_color, &n.message);
            ui.horizontal(|ui| {
                if let Some(u) = &n.link_url {
                    if Self::primary(ui, &n.link_label).clicked() {
                        action = Action::OpenUrl(u.clone());
                    }
                }
                if ui.button("Dismiss").clicked() {
                    self.notice = None;
                }
            });
        } else if !self.last_charge.is_empty() {
            ui.label(RichText::new(&self.last_charge).weak());
        }
        action
    }
}

/// Run `f` on a thread and get its answer later. The host polls the receiver each frame.
pub fn spawn<T: Send + 'static>(
    ctx: &egui::Context,
    f: impl FnOnce() -> T + Send + 'static,
) -> Receiver<T> {
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let r = f();
        let _ = tx.send(r);
        ctx.request_repaint();
    });
    rx
}
