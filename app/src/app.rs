//! The application: state, messages, and the one `match` every change goes through.
//!
//! This file never learns which display it is on. `desktop.rs` and `kiosk.rs`
//! each own a loop that feeds [`App::update`] its input and paints the tree;
//! everything between the input and the pixels is here.

use std::time::{Duration, Instant};

use denise::{ElementState, InputEvent, KeyCode, Role, Size, Theme};
use denise_ui::widgets::{Select, Slider, TextInput, Toggle, open_select};
use denise_ui::{FontId, NodeId, Ui};
use hansolo_core::{Config, ThemePreference, WorkSource};
use hansolo_engine::Miner;

use crate::fonts::Faces;
use crate::store::{State, Store};
use crate::theme::SystemTheme;
use crate::view::{self, CPU_BACKENDS, Nodes, PAGE_SETTINGS, PRESETS, Styles};
use crate::widgets::{Text, Tone};

pub use denise_ui::widgets::ClipboardRequest;

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Tab(usize),
    StartStop,
    Detect,
    OpenTheme,
    Theme(usize),
    OpenSource,
    Source(usize),
    OpenPreset,
    Preset(usize),
    OpenCpuBackend,
    CpuBackend(usize),
    CpuEnabled(bool),
    LowPriority(bool),
    GpuEnabled(bool),
    Intensity(f32),
    AsicEnabled(bool),
    Usb(bool),
    Autostart(bool),
    Save,
    SaveRestart,
    Revert,
    Clipboard(ClipboardRequest),
}

/// What a clipboard is on this machine, supplied by the backend.
pub trait Clipboard {
    fn get(&mut self) -> Option<String>;
    fn set(&mut self, text: &str);
}

/// A clipboard that only remembers within the process: all a bare display has.
#[derive(Default)]
pub struct LocalClipboard(Option<String>);

impl Clipboard for LocalClipboard {
    fn get(&mut self) -> Option<String> {
        self.0.clone()
    }
    fn set(&mut self, text: &str) {
        self.0 = Some(text.to_string());
    }
}

pub struct App {
    pub ui: Ui<Message>,
    pub nodes: Nodes,
    pub styles: Styles,
    pub miner: Miner,
    pub exit: bool,
    store: Store,
    state: State,
    /// What the miner runs with; what Save writes.
    config: Config,
    /// The form's contents that are not text fields (toggles, selects).
    draft: Config,
    system_theme: SystemTheme,
    theme_name: &'static str,
    windowed: bool,
    pub(crate) page: usize,
    started: Instant,
    last_revision: u64,
    last_refresh: Instant,
    last_theme_check: Instant,
    clipboard: Box<dyn Clipboard>,
    pub(crate) cache: crate::refresh::Cache,
    pub(crate) demo: bool,
}

pub struct Setup {
    pub store: Store,
    pub config: Config,
    pub miner: Miner,
    pub faces: Faces,
    pub windowed: bool,
    pub clipboard: Box<dyn Clipboard>,
    /// Show a synthetic miner instead of the engine.
    pub demo: bool,
}

impl App {
    pub fn new(setup: Setup, size: Size, scale: f32) -> Self {
        let Setup { store, config, miner, faces, windowed, clipboard, demo } = setup;
        let scale = config.ui.scale.unwrap_or(scale);
        let system_theme = SystemTheme::start(windowed);
        // Give the sampler a moment so a light desktop does not open dark and flip.
        if windowed && config.ui.theme == ThemePreference::System {
            std::thread::sleep(Duration::from_millis(60));
        }
        let theme = system_theme.resolve(config.ui.theme);
        let mut ui: Ui<Message> = Ui::new(size, theme.scaled(scale));

        let mut bold = None;
        let mut mono = None;
        if let Some(face) = faces.regular {
            eprintln!("font    {}", face.name);
            let id = ui.add_font(face.source);
            ui.set_default_font(id);
        } else {
            eprintln!("font    none found; using the built-in bitmap font");
        }
        if let Some(face) = faces.bold {
            bold = Some(ui.add_font(face.source));
        }
        if let Some(face) = faces.mono {
            mono = Some(ui.add_font(face.source));
        }
        let styles = Styles::new(scale, bold, mono.or(Some(FontId::DEFAULT)));

        let state = store.load_state();
        miner.set_best_ever(state.best_ever_difficulty);
        let nodes = view::build(&mut ui, &styles, &config, 0, windowed);
        let now = Instant::now();
        let mut app = Self {
            ui,
            nodes,
            styles,
            miner,
            exit: false,
            store,
            state,
            draft: config.clone(),
            config,
            system_theme,
            theme_name: theme.name,
            windowed,
            page: 0,
            started: now,
            last_revision: u64::MAX,
            last_refresh: now - Duration::from_secs(10),
            last_theme_check: now,
            clipboard,
            cache: Default::default(),
            demo,
        };
        if !app.demo {
            app.miner.detect_hardware(&app.config);
        }
        if app.demo {
        } else if app.config.autostart {
            app.start();
        } else if app.config.payout_address.trim().is_empty() {
            app.select_page(PAGE_SETTINGS);
            app.ui.toast("Welcome! Enter a payout address to start mining.", Role::Info);
        }
        app.refresh(true);
        app
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// One pass: input, the clock, messages, and the miner's latest state.
    pub fn update(&mut self, events: &[InputEvent]) {
        let mut resized = None;
        for event in events {
            match event {
                InputEvent::SurfaceResized { size, scale_factor } => resized = Some((*size, *scale_factor)),
                InputEvent::Key { code: KeyCode::F5, state: ElementState::Down, .. } => {
                    self.handle(Message::StartStop)
                }
                _ => {}
            }
        }
        self.ui.handle(events);
        if let Some((size, scale)) = resized {
            self.rebuild(size, self.config.ui.scale.unwrap_or(scale));
        }
        self.ui.tick(self.elapsed_ms());

        let messages: Vec<Message> = self.ui.drain_messages().collect();
        for message in messages {
            self.handle(message);
        }

        if self.last_theme_check.elapsed() >= Duration::from_millis(500) {
            self.last_theme_check = Instant::now();
            self.apply_theme();
        }

        let revision = self.miner.revision();
        if revision != self.last_revision && self.last_refresh.elapsed() >= Duration::from_millis(250)
            || self.last_refresh.elapsed() >= Duration::from_secs(1)
        {
            self.last_revision = revision;
            self.refresh(false);
        }
    }

    /// How long a loop may sleep: the tree's own deadline, or the next refresh.
    pub fn next_wake_in(&self) -> Duration {
        let now = self.elapsed_ms();
        let tree = self.ui.next_wake_ms().map(|at| Duration::from_millis(at.saturating_sub(now)));
        let refresh = Duration::from_millis(if self.miner.is_running() { 500 } else { 1000 });
        tree.map_or(refresh, |t| t.min(refresh))
    }

    /// Keys the application claims before the tree, from either backend.
    pub fn wants_exit_on_escape(&self) -> bool {
        !self.ui.popup_open() && self.ui.focused().is_none()
    }

    fn handle(&mut self, message: Message) {
        match message {
            Message::Tab(i) => self.select_page(i),
            Message::StartStop if self.demo => self.ui.toast("This is a demo; run without --demo to mine.", Role::Info),
            Message::StartStop => {
                if self.miner.is_running() {
                    self.miner.stop();
                    self.ui.toast("Mining stopped.", Role::Neutral);
                } else {
                    self.start();
                }
            }
            Message::Detect => self.miner.detect_hardware(&self.config),
            Message::OpenTheme => {
                let from = if self.page == PAGE_SETTINGS { self.nodes.form.theme } else { self.nodes.theme_select };
                open_select(&mut self.ui, from, Message::Theme);
            }
            Message::Theme(i) => {
                self.ui.close_popup();
                let preference = [ThemePreference::System, ThemePreference::Dark, ThemePreference::Light][i.min(2)];
                self.draft.ui.theme = preference;
                self.config.ui.theme = preference;
                for id in [self.nodes.theme_select, self.nodes.form.theme] {
                    self.set_select(id, i);
                }
                self.apply_theme();
                if let Err(e) = self.store.save_config(&self.config) {
                    eprintln!("could not save theme: {e}");
                }
            }
            Message::OpenSource => {
                open_select(&mut self.ui, self.nodes.form.source, Message::Source);
            }
            Message::Source(i) => {
                self.ui.close_popup();
                self.set_select(self.nodes.form.source, i);
                let node = i == 1;
                self.ui.set_visible(self.nodes.form.pool_group, !node);
                self.ui.set_visible(self.nodes.form.preset, !node);
                self.ui.set_visible(self.nodes.form.node_group, node);
            }
            Message::OpenPreset => {
                open_select(&mut self.ui, self.nodes.form.preset, Message::Preset);
            }
            Message::Preset(i) => {
                self.ui.close_popup();
                self.set_select(self.nodes.form.preset, i);
                let url = PRESETS[i].1;
                if !url.is_empty()
                    && let Some(field) = self.ui.widget_mut::<TextInput<Message>>(self.nodes.form.url)
                {
                    field.set_text(url);
                }
            }
            Message::OpenCpuBackend => {
                open_select(&mut self.ui, self.nodes.form.cpu_backend, Message::CpuBackend);
            }
            Message::CpuBackend(i) => {
                self.ui.close_popup();
                self.set_select(self.nodes.form.cpu_backend, i);
            }
            Message::CpuEnabled(on) => self.draft.cpu.enabled = on,
            Message::LowPriority(on) => self.draft.cpu.low_priority = on,
            Message::GpuEnabled(on) => self.draft.gpu.enabled = on,
            Message::Intensity(v) => {
                self.draft.gpu.intensity = v.round().clamp(1.0, 10.0) as u8;
                let label = format!("GPU intensity {}", self.draft.gpu.intensity);
                crate::refresh::set_text(&mut self.ui, self.nodes.form.intensity_label, &label);
            }
            Message::AsicEnabled(on) => self.draft.asic.enabled = on,
            Message::Usb(on) => self.draft.asic.usb = on,
            Message::Autostart(on) => self.draft.autostart = on,
            Message::Save => {
                self.save();
            }
            Message::SaveRestart => {
                if self.save() {
                    self.miner.stop();
                    self.start();
                }
            }
            Message::Revert => {
                self.draft = self.config.clone();
                let size = self.ui.size();
                self.rebuild(size, self.styles.scale);
                self.form_message("Reverted to the saved settings.", Tone::Muted);
            }
            Message::Clipboard(request) => self.clipboard(request),
        }
    }

    fn start(&mut self) {
        if self.config.payout_address.trim().is_empty() {
            self.select_page(PAGE_SETTINGS);
            self.ui.focus(Some(self.nodes.form.address));
            self.ui.toast("Enter a payout address first.", Role::Warning);
            return;
        }
        match self.miner.start(self.config.clone()) {
            Ok(()) => {
                self.ui.toast("Mining started. Good luck!", Role::Success);
                if self.page == PAGE_SETTINGS {
                    self.select_page(0);
                }
            }
            Err(e) => self.ui.toast_for(format!("Could not start: {e}"), Role::Error, 6000),
        }
        self.refresh(true);
    }

    fn select_page(&mut self, page: usize) {
        self.page = page.min(self.nodes.pages.len() - 1);
        for (i, &node) in self.nodes.pages.iter().enumerate() {
            self.ui.set_visible(node, i == self.page);
        }
        if let Some(tabs) = self.ui.widget::<denise_ui::widgets::Tabs<Message>>(self.nodes.tabs)
            && tabs.selected() != self.page
            && let Some(tabs) = self.ui.widget_mut::<denise_ui::widgets::Tabs<Message>>(self.nodes.tabs)
        {
            tabs.set_selected(self.page);
        }
        self.refresh(true);
    }

    fn set_select(&mut self, id: NodeId, index: usize) {
        if let Some(select) = self.ui.widget_mut::<Select<Message>>(id) {
            select.set_selected(Some(index));
        }
    }

    /// Follows the theme preference, and the system when that is the preference.
    fn apply_theme(&mut self) {
        let theme: Theme = self.system_theme.resolve(self.config.ui.theme);
        if theme.name != self.theme_name {
            self.theme_name = theme.name;
            self.ui.set_theme(theme.scaled(self.styles.scale));
        }
    }

    /// Throws the tree away and builds it again at a new size, keeping the page,
    /// the focus-free parts of the form, and nothing else.
    fn rebuild(&mut self, size: Size, scale: f32) {
        self.read_form();
        self.ui.close_popup();
        self.ui.remove(self.nodes.container);
        if (scale - self.styles.scale).abs() > f32::EPSILON {
            let bold = Some(self.styles.title.font);
            let mono = Some(self.styles.mono.font);
            self.styles = Styles::new(scale, bold, mono);
            self.ui.set_theme(self.system_theme.resolve(self.config.ui.theme).scaled(scale));
        }
        let _ = size;
        self.nodes = view::build(&mut self.ui, &self.styles, &self.draft, self.page, self.windowed);
        self.cache = Default::default();
        self.refresh(true);
    }

    /// Copies the text fields into the draft.
    fn read_form(&mut self) {
        let f = &self.nodes.form;
        let read = |ui: &Ui<Message>, id: NodeId| {
            ui.widget::<TextInput<Message>>(id).map(|w| w.text().trim().to_string()).unwrap_or_default()
        };
        let selected = |ui: &Ui<Message>, id: NodeId| ui.widget::<Select<Message>>(id).and_then(|s| s.selected()).unwrap_or(0);
        let checked = |ui: &Ui<Message>, id: NodeId| ui.widget::<Toggle<Message>>(id).is_some_and(|t| t.checked());

        let ui = &self.ui;
        let d = &mut self.draft;
        d.payout_address = read(ui, f.address);
        d.worker_name = read(ui, f.worker);
        d.source = if selected(ui, f.source) == 1 {
            let cookie = read(ui, f.cookie);
            WorkSource::Node {
                rpc_url: read(ui, f.rpc_url),
                rpc_user: read(ui, f.rpc_user),
                rpc_password: read(ui, f.rpc_password),
                cookie_file: (!cookie.is_empty()).then_some(cookie),
                poll_secs: match &self.config.source {
                    WorkSource::Node { poll_secs, .. } => *poll_secs,
                    _ => 5,
                },
            }
        } else {
            let username = read(ui, f.username);
            let password = read(ui, f.password);
            WorkSource::Stratum {
                url: read(ui, f.url),
                username: (!username.is_empty()).then_some(username),
                password: if password.is_empty() { "x".into() } else { password },
            }
        };
        d.cpu.enabled = checked(ui, f.cpu);
        d.cpu.low_priority = checked(ui, f.low_priority);
        d.cpu.threads = read(ui, f.threads).parse().ok().filter(|&t: &usize| t > 0);
        d.cpu.backend = match selected(ui, f.cpu_backend) {
            0 => None,
            i => Some(CPU_BACKENDS[i].to_string()),
        };
        d.gpu.enabled = checked(ui, f.gpu);
        d.gpu.intensity = ui.widget::<Slider<Message>>(f.intensity).map_or(d.gpu.intensity, |s| s.value().round() as u8);
        d.asic.enabled = checked(ui, f.asic);
        d.asic.usb = checked(ui, f.usb);
        d.asic.network_devices =
            read(ui, f.network_devices).split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        d.autostart = checked(ui, f.autostart);
    }

    /// Validates and writes the form. Returns whether it was saved.
    fn save(&mut self) -> bool {
        self.read_form();
        let problem = if self.draft.payout_address.is_empty() {
            Some("A payout address is required.")
        } else if !plausible_address(&self.draft.payout_address) {
            Some("That does not look like a Bitcoin address.")
        } else {
            match &self.draft.source {
                WorkSource::Stratum { url, .. } if url.is_empty() => Some("The pool needs a URL."),
                WorkSource::Node { rpc_url, .. } if rpc_url.is_empty() => Some("The node needs an RPC URL."),
                _ => None,
            }
        };
        if let Some(problem) = problem {
            self.form_message(problem, Tone::Role(Role::Error));
            return false;
        }
        self.config = self.draft.clone();
        match self.store.save_config(&self.config) {
            Ok(()) => {
                let path = self.store.config_path().display().to_string();
                self.form_message(&format!("Saved to {path}"), Tone::Role(Role::Success));
            }
            Err(e) => self.form_message(&format!("Applied, but not saved: {e}"), Tone::Role(Role::Warning)),
        }
        true
    }

    fn form_message(&mut self, text: &str, tone: Tone) {
        let id = self.nodes.form.message;
        if let Some(widget) = self.ui.widget_mut::<Text>(id) {
            widget.set_text(text);
            widget.set_tone(tone);
        }
    }

    fn clipboard(&mut self, request: ClipboardRequest) {
        match request {
            ClipboardRequest::Copy(text) | ClipboardRequest::Cut(text) => self.clipboard.set(&text),
            ClipboardRequest::Paste => {
                let Some(text) = self.clipboard.get() else { return };
                let text = text.replace(['\n', '\r'], "");
                if let Some(focused) = self.ui.focused()
                    && let Some(field) = self.ui.widget_mut::<TextInput<Message>>(focused)
                {
                    field.insert_text(&text);
                }
            }
        }
    }

    fn refresh(&mut self, force: bool) {
        self.last_refresh = Instant::now();
        let snapshot = if self.demo {
            crate::demo::snapshot(self.started.elapsed().as_secs())
        } else {
            self.miner.snapshot()
        };
        let best = snapshot.shares.best_difficulty.max(snapshot.shares.best_ever_difficulty);
        if best > self.state.best_ever_difficulty * 1.000_001 {
            self.state.best_ever_difficulty = best;
            if let Err(e) = self.store.save_state(&self.state) {
                eprintln!("could not save state: {e}");
            }
        }
        crate::refresh::apply(self, &snapshot, force);
    }

    pub fn show_page(&mut self, page: usize) {
        self.select_page(page);
    }

}

/// A shape check, not validation: the engine parses the address properly.
fn plausible_address(address: &str) -> bool {
    let lower = address.to_ascii_lowercase();
    let bech32 = ["bc1", "tb1", "bcrt1"].iter().any(|p| lower.starts_with(p));
    let base58 = address.starts_with(['1', '3', 'm', 'n', '2']);
    (bech32 || base58) && (26..=90).contains(&address.len()) && address.chars().all(|c| c.is_ascii_alphanumeric())
}
