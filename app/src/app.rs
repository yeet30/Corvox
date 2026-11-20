use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver};
use eframe::egui::{self, Align, Color32, CornerRadius, FontId, Key, Layout, RichText, Sense, Stroke, UiBuilder, Vec2};

use crate::audio::{self, Player};
use crate::catalog::{find_engines_dir, find_uv, Catalog, Engine};
use crate::config::{self, Config};
use crate::engine::{self, CancelHandle, Event, Helper, Tx};
use crate::icons::{self, Icon};
use crate::platform;
use crate::ptt::{self, Ptt};

pub const APP_NAME: &str = "Corvox";
pub const EXPANDED: Vec2 = Vec2::new(440.0, 400.0);
pub const COLLAPSED: Vec2 = Vec2::new(460.0, 56.0);

/// Streamed audio is held back until this much is ready, so slow CPUs don't stutter mid-word.
const PREBUFFER_SECS: f32 = 0.4;

const RED: Color32 = Color32::from_rgb(0xE2, 0x4B, 0x4A);
const AMBER: Color32 = Color32::from_rgb(0xEF, 0x9F, 0x27);
const GREEN: Color32 = Color32::from_rgb(0x1D, 0x9E, 0x75);
const GOLD: Color32 = Color32::from_rgb(0xBA, 0x75, 0x17);

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Main,
    Engines,
    Voices,
    Settings,
}

struct Install {
    started: Instant,
    mb: f32,
    line: String,
    handle: CancelHandle,
}

/// Audio of the current request waiting for the prebuffer to fill.
struct Pending {
    req: u64,
    chunks: Vec<(Vec<f32>, u32)>,
    secs: f32,
    playing: bool,
}

pub struct App {
    cfg: Config,
    catalog: Option<Catalog>,
    engines_dir: Option<PathBuf>,
    uv: Option<PathBuf>,
    rx: Receiver<Event>,
    tx: Tx,

    helper: Option<Helper>,
    helper_ready: bool,
    queued: Option<String>,
    current_req: Option<u64>,
    pending: Option<Pending>,
    loaded: HashMap<String, String>,
    loading: Option<String>,
    model_dl: HashMap<String, f32>,

    player: Player,
    devices: Vec<String>,
    ptt: Ptt,
    quiet_since: Option<Instant>,

    page: Page,
    text: String,
    history_pos: Option<usize>,
    focus_text: bool,
    status: String,
    error: Option<String>,
    show_all_engines: bool,
    installs: HashMap<String, Install>,
    benching: HashMap<String, String>,
    agree_prompt: Option<String>,
    voices: Vec<String>,
    importing: bool,
    hotkey_text: String,
    hf_input: String,
    hf_msg: Option<String>,
    hotkey_mgr: Option<global_hotkey::GlobalHotKeyManager>,
    hotkey_cur: Option<global_hotkey::hotkey::HotKey>,
    frames: u32,
    win: Option<platform::Window>,
    scroll_to: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, cfg: Config) -> Self {
        let ctx = cc.egui_ctx.clone();
        egui_extras::install_image_loaders(&ctx);
        style(&ctx);
        let (s, rx) = unbounded();
        let tx: Tx = (s, ctx.clone());

        let engines_dir = find_engines_dir();
        let catalog = engines_dir.as_ref().and_then(|d| Catalog::load(d).ok());
        let devices = audio::output_device_names();
        let mic = cfg.output_device.clone().or_else(|| audio::guess_virtual_device(&devices));
        let player = Player::new(mic.as_deref(), cfg.monitor, cfg.monitor_volume);

        // Global hotkey events arrive on their own channel; forward them so the UI wakes up.
        let htx = tx.clone();
        std::thread::spawn(move || {
            while let Ok(e) = global_hotkey::GlobalHotKeyEvent::receiver().recv() {
                if e.state == global_hotkey::HotKeyState::Pressed {
                    let _ = htx.0.send(Event::Hotkey);
                    htx.1.request_repaint();
                }
            }
        });

        let mut app = Self {
            hotkey_text: cfg.hotkey.clone(),
            cfg,
            catalog,
            engines_dir,
            uv: find_uv(),
            rx,
            tx,
            helper: None,
            helper_ready: false,
            queued: None,
            current_req: None,
            pending: None,
            loaded: HashMap::new(),
            loading: None,
            model_dl: HashMap::new(),
            player,
            devices,
            ptt: Ptt::new(),
            quiet_since: None,
            page: Page::Main,
            text: String::new(),
            history_pos: None,
            focus_text: true,
            status: "Ready".into(),
            error: None,
            show_all_engines: false,
            installs: HashMap::new(),
            benching: HashMap::new(),
            agree_prompt: None,
            voices: config::list_voices(),
            importing: false,
            hf_input: String::new(),
            hf_msg: None,
            hotkey_mgr: global_hotkey::GlobalHotKeyManager::new().ok(),
            hotkey_cur: None,
            frames: 0,
            win: platform::Window::from(cc),
            scroll_to: None,
        };
        if app.cfg.voice.as_ref().map(|v| !app.voices.contains(v)).unwrap_or(true) {
            app.cfg.voice = app.voices.first().cloned();
        }
        if let Some(w) = app.win {
            w.round_corners();
        }
        app.apply_hotkey();
        app.fix_engine_for_language();
        app.ensure_helper();
        if let Ok(t) = std::env::var("CORVOX_SAY") {
            app.speak(t); // debug: speak on startup
        }
        if let Ok(env) = std::env::var("CORVOX_INSTALL") {
            app.start_install(&env); // debug: start an install on launch
            app.page = Page::Engines;
            app.show_all_engines = true;
        }
        match std::env::var("CORVOX_SHOT_PAGE").as_deref() {
            Ok("engines") => app.page = Page::Engines,
            Ok("voices") => app.page = Page::Voices,
            Ok("settings") => app.page = Page::Settings,
            _ => {}
        }
        app
    }

    // ---------- state helpers ----------

    fn engine(&self) -> Option<&Engine> {
        self.catalog.as_ref()?.engine(&self.cfg.engine)
    }

    fn agreed(&self, id: &str) -> bool {
        self.cfg.agreed.iter().any(|a| a == id)
    }

    fn busy(&self) -> bool {
        self.current_req.is_some() || self.player.is_playing() || self.pending.is_some()
    }

    fn apply_hotkey(&mut self) {
        let Some(mgr) = &self.hotkey_mgr else { return };
        if let Some(old) = self.hotkey_cur.take() {
            let _ = mgr.unregister(old);
        }
        match self.cfg.hotkey.parse::<global_hotkey::hotkey::HotKey>() {
            Ok(hk) => match mgr.register(hk) {
                Ok(()) => self.hotkey_cur = Some(hk),
                Err(e) => self.error = Some(format!("Couldn't register {}: {e}", self.cfg.hotkey)),
            },
            Err(e) => self.error = Some(format!("Bad shortcut \"{}\": {e}", self.cfg.hotkey)),
        }
    }

    fn fix_engine_for_language(&mut self) {
        let Some(cat) = &self.catalog else { return };
        let ok = cat.engine(&self.cfg.engine).map(|e| e.languages.contains(&self.cfg.language)).unwrap_or(false);
        if !ok {
            let list = cat.engines_for(&self.cfg.language);
            let pick = list.iter().find(|e| engine::is_installed(&e.env)).or(list.first());
            if let Some(e) = pick {
                self.cfg.engine = e.id.clone();
            }
        }
    }

    /// Starts (or swaps) the helper process for the selected engine's environment.
    fn ensure_helper(&mut self) {
        let Some(e) = self.engine().cloned() else { return };
        if !engine::is_installed(&e.env) {
            self.helper = None;
            return;
        }
        let alive = self.helper.as_mut().map(|h| h.env == e.env && h.is_alive()).unwrap_or(false);
        if alive {
            if self.helper_ready {
                self.preload(false);
            }
            return;
        }
        let (Some(uv), Some(dir)) = (self.uv.clone(), self.engines_dir.clone()) else { return };
        self.helper = None;
        self.helper_ready = false;
        self.loaded.clear();
        self.loading = None;
        match Helper::spawn(&uv, &dir, &e.env, self.agreed(&e.id), self.cfg.int8, self.tx.clone()) {
            Ok(h) => {
                self.helper = Some(h);
                self.status = "Starting engine…".into();
            }
            Err(err) => self.error = Some(format!("Couldn't start engine: {err}")),
        }
    }

    fn restart_helper(&mut self) {
        self.helper = None;
        self.helper_ready = false;
        self.ensure_helper();
    }

    /// Loads the selected model (downloading it the first time) and prepares the voice.
    fn preload(&mut self, force: bool) {
        let Some(e) = self.engine().cloned() else { return };
        let key = format!("{}:{}", e.id, self.cfg.language);
        if !force && (self.loaded.contains_key(&key) || self.loading.as_deref() == Some(key.as_str())) {
            return;
        }
        let voice = self.cfg.voice_path();
        let prefix = format!("{}:", e.id);
        let others: Vec<String> = self.loaded.keys().filter(|k| !k.starts_with(&prefix)).cloned().collect();
        if let Some(h) = &mut self.helper {
            for k in others {
                if let Some(model) = k.split(':').next() {
                    h.unload(model);
                }
            }
            self.loaded.retain(|k, _| k.starts_with(&prefix));
            if h.load(&e.id, &self.cfg.language, voice.as_deref(), e.model_mb).is_ok() {
                self.loading = Some(key);
                self.status = "Loading the model…".into();
            }
        }
    }

    fn speak(&mut self, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.error = None;
        let Some(voice) = self.cfg.voice_path() else {
            self.error = Some("Add your voice clip first.".into());
            self.page = Page::Voices;
            self.expand(true);
            return;
        };
        let Some(e) = self.engine().cloned() else { return };
        if !engine::is_installed(&e.env) {
            self.error = Some(format!("Install {} first.", e.name));
            self.page = Page::Engines;
            self.expand(true);
            return;
        }
        self.cfg.push_recent(&text);
        self.cfg.save();
        if self.busy() {
            self.stop(); // a new line interrupts the old one
        }
        self.ensure_helper();
        if !self.helper_ready {
            self.queued = Some(text);
            return;
        }
        if let Some(h) = &mut self.helper {
            match h.speak(&e.id, &text, &voice, &self.cfg.language, e.model_mb) {
                Ok(id) => {
                    self.current_req = Some(id);
                    self.pending = Some(Pending { req: id, chunks: vec![], secs: 0.0, playing: false });
                    self.status = "Generating…".into();
                }
                Err(err) => self.error = Some(format!("Engine error: {err}")),
            }
        }
    }

    fn stop(&mut self) {
        if self.current_req.is_some() {
            if let Some(h) = &mut self.helper {
                h.cancel();
            }
        }
        self.current_req = None;
        self.pending = None;
        self.queued = None;
        self.player.stop();
        self.ptt.release();
        self.status = "Ready".into();
    }

    fn play_now(&mut self, chunks: Vec<(Vec<f32>, u32)>) {
        if self.cfg.ptt_key.is_some() && !self.ptt.is_held() {
            self.ptt.press(self.cfg.ptt_key.as_deref());
            self.player.push_silence(0.15); // give Discord a moment to open the mic
        }
        for (s, sr) in chunks {
            self.player.push(&s, sr);
        }
        self.status = "Speaking".into();
    }

    fn rebuild_player(&mut self) {
        self.devices = audio::output_device_names();
        let mic = self.cfg.output_device.clone().or_else(|| audio::guess_virtual_device(&self.devices));
        self.player = Player::new(mic.as_deref(), self.cfg.monitor, self.cfg.monitor_volume);
    }

    fn expand(&mut self, expanded: bool) {
        if self.cfg.collapsed == !expanded {
            return;
        }
        self.cfg.collapsed = !expanded;
        self.cfg.save();
        let size = if expanded { EXPANDED } else { COLLAPSED };
        self.tx.1.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        self.focus_text = true;
    }

    fn set_on_top(&mut self, on: bool) {
        self.cfg.always_on_top = on;
        self.cfg.save();
        let lvl = if on { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
        self.tx.1.send_viewport_cmd(egui::ViewportCommand::WindowLevel(lvl));
    }

    fn import_voice(&mut self) {
        let Some(src) = rfd::FileDialog::new()
            .set_title("Pick a clip of your voice")
            .add_filter("Audio", &["mp3", "wav", "flac", "ogg", "m4a", "aac", "opus", "webm", "mp4"])
            .pick_file()
        else {
            return;
        };
        let stem: String = src
            .file_stem()
            .map(|s| s.to_string_lossy().chars().map(|c| if c.is_alphanumeric() || c == '-' || c == ' ' { c } else { '_' }).collect())
            .unwrap_or_else(|| "voice".into());
        let mut name = stem.trim().to_string();
        let mut n = 2;
        while config::voices_dir().join(format!("{name}.wav")).exists() {
            name = format!("{stem} {n}");
            n += 1;
        }
        self.importing = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let dest = config::voices_dir().join(format!("{name}.wav"));
            let r = audio::import_voice(&src, &dest).map_err(|e| e.to_string());
            let _ = tx.0.send(Event::Status(match &r {
                Ok(secs) => format!("voice-ok:{name}:{secs:.1}"),
                Err(e) => format!("voice-err:{e}"),
            }));
            tx.1.request_repaint();
        });
    }

    fn after_install(&mut self, env: &str) {
        let Some(cat) = self.catalog.clone() else { return };
        // Select the freshly installed engine if it speaks the current language, then load it
        // right away so the model download happens now (with progress), not on first use.
        if let Some(e) = cat.engines_for(&self.cfg.language).into_iter().find(|e| e.env == env) {
            self.cfg.engine = e.id.clone();
            self.cfg.save();
        }
        if self.engine().map(|e| e.env == env).unwrap_or(false) {
            self.restart_helper();
        }
    }

    fn handle_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::HelperReady { env } => {
                    if self.helper.as_ref().map(|h| h.env == env).unwrap_or(false) {
                        self.helper_ready = true;
                        self.status = "Ready".into();
                        self.preload(false);
                        if let Some(t) = self.queued.take() {
                            self.speak(t);
                        }
                    }
                }
                Event::HelperExited { env, msg } => {
                    if self.helper.as_ref().map(|h| h.env == env).unwrap_or(false) {
                        self.helper = None;
                        self.helper_ready = false;
                        self.loaded.clear();
                        self.loading = None;
                        self.error = Some(format!("Engine stopped. {msg}"));
                        self.status = "Ready".into();
                    }
                }
                Event::Status(s) => {
                    if let Some(rest) = s.strip_prefix("voice-ok:") {
                        self.importing = false;
                        let mut it = rest.rsplitn(2, ':');
                        let secs = it.next().unwrap_or("");
                        let name = it.next().unwrap_or("").to_string();
                        self.voices = config::list_voices();
                        self.cfg.voice = Some(name.clone());
                        self.cfg.save();
                        self.error = None;
                        self.status = format!("Added \"{name}\" ({secs} s)");
                        self.preload(true);
                    } else if let Some(e) = s.strip_prefix("voice-err:") {
                        self.importing = false;
                        self.error = Some(format!("Couldn't use that file: {e}"));
                    } else {
                        self.status = s;
                    }
                }
                Event::LoadDone { model, result } => {
                    let key = self.loading.take().unwrap_or_else(|| format!("{model}:{}", self.cfg.language));
                    self.model_dl.remove(&model);
                    match result {
                        Ok(dev) => {
                            self.loaded.insert(key, dev);
                            if self.current_req.is_none() {
                                self.status = "Ready".into();
                            }
                        }
                        Err(e) => {
                            self.error = Some(e);
                            self.status = "Ready".into();
                        }
                    }
                }
                Event::ModelProgress { model, done_mb } => {
                    self.model_dl.insert(model, done_mb);
                }
                Event::Chunk { req, samples, sr } => {
                    let mut flush = None;
                    if let Some(p) = &mut self.pending {
                        if p.req == req {
                            if p.playing {
                                flush = Some(vec![(samples, sr)]);
                            } else {
                                p.secs += samples.len() as f32 / sr as f32;
                                p.chunks.push((samples, sr));
                                if p.secs >= PREBUFFER_SECS {
                                    p.playing = true;
                                    flush = Some(std::mem::take(&mut p.chunks));
                                }
                            }
                        }
                    }
                    if let Some(c) = flush {
                        self.play_now(c);
                    }
                }
                Event::SpeakDone { req, result } => {
                    if self.current_req == Some(req) {
                        self.current_req = None;
                        if let Some(p) = self.pending.take() {
                            if p.req == req && !p.chunks.is_empty() {
                                self.play_now(p.chunks);
                            }
                        }
                        if let Err(e) = result {
                            self.error = Some(e);
                        }
                    }
                }
                Event::InstallLine { env, line } => {
                    if let Some(i) = self.installs.get_mut(&env) {
                        let l = line.trim();
                        if !l.is_empty() {
                            i.line = l.to_string();
                        }
                    }
                }
                Event::InstallProgress { env, mb } => {
                    if let Some(i) = self.installs.get_mut(&env) {
                        i.mb = i.mb.max(mb);
                    }
                }
                Event::InstallDone { env, ok, cancelled } => {
                    let last = self.installs.remove(&env).map(|i| i.line).unwrap_or_default();
                    if ok {
                        self.status = "Engine installed".into();
                        self.after_install(&env);
                    } else if cancelled {
                        self.status = "Install cancelled".into();
                    } else {
                        self.error = Some(format!("Install failed: {last}"));
                    }
                }
                Event::BenchStatus { model, msg } => {
                    self.benching.insert(model, msg);
                }
                Event::BenchDone { model, result } => {
                    self.benching.remove(&model);
                    match result {
                        Ok(r) => {
                            self.cfg.bench.insert(model, r);
                            self.cfg.save();
                        }
                        Err(e) => self.error = Some(format!("Test failed: {e}")),
                    }
                }
                Event::Hotkey => {
                    let ctx = &self.tx.1;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    self.focus_text = true;
                }
            }
        }
    }

    fn update_ptt(&mut self) {
        let idle = !self.busy();
        if idle && !self.ptt.is_held() && (self.status == "Speaking" || self.status == "Generating…") {
            self.status = "Ready".into();
        }
        if !self.ptt.is_held() {
            return;
        }
        if !idle {
            self.quiet_since = None;
        } else {
            let since = *self.quiet_since.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_millis(250) {
                self.ptt.release();
                self.quiet_since = None;
                if self.status == "Speaking" {
                    self.status = "Ready".into();
                }
            }
        }
    }

    // ---------- UI pieces ----------

    fn title_bar(&mut self, ui: &mut egui::Ui) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
        // The drag area goes first so the buttons drawn on top of it get the clicks.
        let drag = ui.interact(rect, ui.scope_id().with("titlebar"), Sense::click_and_drag());
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        ui.scope_builder(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)), |ui| {
            icons::small(ui, Icon::Mic, ui.visuals().weak_text_color());
            ui.label(RichText::new(APP_NAME).color(ui.visuals().weak_text_color()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icons::button(ui, Icon::Close, "Close").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                let on_top = self.cfg.always_on_top;
                let (icon, tip) = if on_top { (Icon::Pin, "On top of other windows (click to turn off)") } else { (Icon::PinOff, "Keep on top of other windows") };
                let tint = if on_top { ui.visuals().selection.stroke.color } else { ui.visuals().weak_text_color() };
                if icons::button_tinted(ui, icon, tint, tip).clicked() {
                    self.set_on_top(!on_top);
                }
                if icons::button(ui, Icon::ChevronDown, "Collapse to a bar").clicked() {
                    self.expand(false);
                }
                let st_tint = if self.page == Page::Settings { ui.visuals().selection.stroke.color } else { ui.visuals().widgets.inactive.fg_stroke.color };
                if icons::button_tinted(ui, Icon::Sliders, st_tint, "Settings").clicked() {
                    self.page = if self.page == Page::Settings { Page::Main } else { Page::Settings };
                }
            });
        });
    }

    fn text_box(&mut self, ui: &mut egui::Ui, width: f32) {
        let up = ui.input(|i| i.key_pressed(Key::ArrowUp));
        let down = ui.input(|i| i.key_pressed(Key::ArrowDown));
        let resp = ui.add(
            egui::TextEdit::singleline(&mut self.text)
                .hint_text("Type and press Enter…")
                .font(FontId::proportional(16.0))
                .desired_width(width)
                .margin(Vec2::new(8.0, 7.0)),
        );
        if self.focus_text {
            resp.request_focus();
            self.focus_text = false;
        }
        if resp.has_focus() && (up || down) {
            let list = self.phrase_list();
            if !list.is_empty() {
                let pos = match (self.history_pos, up) {
                    (None, true) => Some(0),
                    (None, false) => None,
                    (Some(p), true) => Some((p + 1).min(list.len() - 1)),
                    (Some(0), false) => {
                        self.text.clear();
                        None
                    }
                    (Some(p), false) => Some(p - 1),
                };
                self.history_pos = pos;
                if let Some(p) = pos {
                    self.text = list[p].clone();
                }
            }
        }
        if resp.changed() {
            self.history_pos = None;
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            let t = std::mem::take(&mut self.text);
            self.history_pos = None;
            self.speak(t);
            self.focus_text = true;
        }
        if resp.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
            if self.busy() {
                self.stop();
            } else {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
        }
    }

    /// Pinned phrases first, then recent ones.
    fn phrase_list(&self) -> Vec<String> {
        let mut v = self.cfg.pinned.clone();
        v.extend(self.cfg.recent.iter().filter(|r| !self.cfg.pinned.contains(r)).cloned());
        v
    }

    /// A short status for the current engine, including model download progress.
    fn status_text(&self) -> String {
        if let Some(e) = self.engine() {
            if let Some(done) = self.model_dl.get(&e.id) {
                return format!("Downloading model {}", pct_or_mb(*done, e.model_mb));
            }
        }
        self.status.clone()
    }

    fn status_chip(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        if busy && icons::labeled(ui, Icon::Stop, "Stop").on_hover_text("Stop speaking (Esc)").clicked() {
            self.stop();
        }
        let text = self.status_text();
        let col = if self.error.is_some() {
            RED
        } else if busy || text != "Ready" {
            AMBER
        } else {
            GREEN
        };
        ui.label(RichText::new(truncate(&text, 34)).color(col).small());
        let (r, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
        ui.painter().circle_filled(r.center(), 3.5, col);
    }

    fn error_line(&mut self, ui: &mut egui::Ui) {
        if let Some(e) = self.error.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(e).color(RED).small());
                if icons::button(ui, Icon::Close, "Dismiss").clicked() {
                    self.error = None;
                }
            });
        }
    }

    fn main_page(&mut self, ui: &mut egui::Ui) {
        let w = ui.available_width();
        self.text_box(ui, w);
        ui.add_space(2.0);
        self.error_line(ui);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Recent").small().weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new("↑↓ to choose · Enter speaks").small().weak());
            });
        });

        let list = self.phrase_list();
        let bottom_h = 38.0;
        let mut speak = None;
        let mut pin = None;
        let mut remove = None;
        let mut edit = None;
        let history_pos = self.history_pos;
        let pinned_list = self.cfg.pinned.clone();
        egui::ScrollArea::vertical().max_height(ui.available_height() - bottom_h).auto_shrink([false, false]).show(ui, |ui| {
            if list.is_empty() {
                ui.add_space(12.0);
                ui.label(RichText::new("Phrases you speak show up here. Click one to say it again.").weak());
            }
            for (i, p) in list.iter().enumerate() {
                let pinned = pinned_list.contains(p);
                let selected = history_pos == Some(i);
                ui.horizontal(|ui| {
                    if icons::button(ui, Icon::Play, "Speak").clicked() {
                        speak = Some(p.clone());
                    }
                    let avail = ui.available_width() - 52.0;
                    let color = if selected { ui.visuals().selection.stroke.color } else { ui.visuals().text_color() };
                    let label = egui::Label::new(RichText::new(p).color(color)).truncate().sense(Sense::click());
                    let r = ui
                        .allocate_ui_with_layout(Vec2::new(avail, 20.0), Layout::left_to_right(Align::Center), |ui| {
                            ui.set_min_width(avail);
                            ui.add(label)
                        })
                        .inner
                        .on_hover_text("Click to speak, right-click to edit");
                    if r.clicked() {
                        speak = Some(p.clone());
                    }
                    if r.secondary_clicked() {
                        edit = Some(p.clone());
                    }
                    let tint = if pinned { ui.visuals().selection.stroke.color } else { ui.visuals().weak_text_color() };
                    if icons::button_tinted(ui, Icon::Pin, tint, if pinned { "Unpin" } else { "Pin" }).clicked() {
                        pin = Some(p.clone());
                    }
                    if !pinned && icons::button_tinted(ui, Icon::Close, ui.visuals().weak_text_color(), "Remove").clicked() {
                        remove = Some(p.clone());
                    }
                });
            }
        });
        if let Some(p) = edit {
            self.text = p;
            self.focus_text = true;
        }
        if let Some(p) = pin {
            self.cfg.toggle_pin(&p);
            self.cfg.save();
        }
        if let Some(p) = remove {
            self.cfg.recent.retain(|r| *r != p);
            self.cfg.save();
        }
        if let Some(p) = speak {
            self.speak(p);
        }

        ui.separator();
        ui.horizontal(|ui| {
            let voice = self.cfg.voice.clone().unwrap_or_else(|| "Add voice".into());
            if icons::labeled(ui, Icon::User, &truncate(&voice, 14)).on_hover_text("Voices").clicked() {
                self.page = Page::Voices;
            }
            self.language_combo(ui);
            let ename = self.engine().map(|e| short_name(&e.name)).unwrap_or_else(|| "Engine".into());
            if icons::labeled(ui, Icon::Cpu, &ename).on_hover_text("Engines and their stats").clicked() {
                self.page = Page::Engines;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| self.status_chip(ui));
        });
    }

    fn language_combo(&mut self, ui: &mut egui::Ui) {
        let Some(cat) = self.catalog.clone() else { return };
        let before = self.cfg.language.clone();
        egui::ComboBox::from_id_salt("lang")
            .selected_text(self.cfg.language.to_uppercase())
            .width(56.0)
            .show_ui(ui, |ui| {
                for code in cat.all_languages() {
                    ui.selectable_value(&mut self.cfg.language, code.clone(), cat.lang_name(&code));
                }
            })
            .response
            .on_hover_text("Language");
        if self.cfg.language != before {
            self.fix_engine_for_language();
            self.cfg.save();
            self.ensure_helper();
        }
    }

    fn back_header(&mut self, ui: &mut egui::Ui, title: &str) {
        ui.horizontal(|ui| {
            if icons::labeled(ui, Icon::Back, "Back").clicked() {
                self.page = Page::Main;
            }
            ui.label(RichText::new(title).strong());
        });
    }

    fn engines_page(&mut self, ui: &mut egui::Ui) {
        let Some(cat) = self.catalog.clone() else {
            self.back_header(ui, "Engines");
            ui.label("Couldn't find engines/catalog.json next to the app.");
            return;
        };
        ui.horizontal(|ui| {
            if icons::labeled(ui, Icon::Back, "Back").clicked() {
                self.page = Page::Main;
            }
            ui.label(RichText::new(format!("Engines · {}", cat.lang_name(&self.cfg.language))).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.checkbox(&mut self.show_all_engines, "All languages");
            });
        });
        if self.uv.is_none() {
            ui.label(RichText::new("uv isn't installed, so engines can't be installed. See the README.").color(RED));
        }
        self.error_line(ui);
        let list: Vec<Engine> = if self.show_all_engines { cat.engines.clone() } else { cat.engines_for(&self.cfg.language).into_iter().cloned().collect() };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for e in &list {
                self.engine_card(ui, &cat, e);
                ui.add_space(4.0);
            }
            ui.label(
                RichText::new("Voice match compares the output to your clip. Clarity is how well Whisper understands it. Speed is seconds of speech per second of work.")
                    .small()
                    .weak(),
            );
        });
    }

    fn engine_card(&mut self, ui: &mut egui::Ui, cat: &Catalog, e: &Engine) {
        let installed = engine::is_installed(&e.env);
        let selected = self.cfg.engine == e.id;
        let speaks_lang = e.languages.contains(&self.cfg.language);
        let stroke = if selected { Stroke::new(1.5, ui.visuals().selection.stroke.color) } else { ui.visuals().widgets.noninteractive.bg_stroke };
        let card = egui::Frame::new().stroke(stroke).corner_radius(CornerRadius::same(8)).inner_margin(8.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let can_select = installed && speaks_lang;
                let r = ui.add_enabled(can_select, egui::RadioButton::new(selected, RichText::new(&e.name).strong()));
                if r.clicked() {
                    self.cfg.engine = e.id.clone();
                    self.cfg.save();
                    self.ensure_helper();
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let (txt, col) = if e.is_gpu() { ("GPU", GOLD) } else { ("CPU", GREEN) };
                    ui.label(RichText::new(txt).small().color(col).strong());
                });
            });
            ui.label(RichText::new(format!("{} · {}", e.by, e.note)).small().weak());
            if e.requires_hf && !config::has_hf_token() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Needs a free Hugging Face login to clone voices.").small().color(GOLD));
                    if ui.small_button("Set up").clicked() {
                        self.page = Page::Settings;
                    }
                });
            }

            let measured = self.cfg.bench.get(&e.id).cloned();
            let src = match &measured {
                Some(b) => format!(
                    "Measured on your PC · {} · voice \"{}\"{}",
                    cat.lang_name(&b.language),
                    b.voice,
                    if b.int8 { " · int8" } else { "" }
                ),
                None => format!("Published · {}", if e.published.source.is_empty() { "no numbers yet" } else { &e.published.source }),
            };
            let b = measured.clone().unwrap_or_default();
            let p = &e.published;
            let pick = |m: Option<f32>, pb: Option<f32>| if measured.is_some() { m } else { pb };
            let vm = pick(b.voice_match, p.voice_match);
            let cl = pick(b.clarity, p.clarity);
            let sp = pick(b.speed, p.speed);
            let ram = pick(b.ram_mb, p.ram_mb);
            let vram = pick(b.vram_mb, p.vram_mb);
            stat_row(ui, "Voice match", vm.map(|v| v / 100.0), vm.map(|v| format!("{v:.0}%")));
            stat_row(ui, "Clarity", cl.map(|v| v / 100.0), cl.map(|v| format!("{v:.0}%")));
            stat_row(ui, "Speed", sp.map(|v| (v / 4.0).min(1.0)), sp.map(|v| if v < 2.0 { format!("{v:.2}× real time") } else { format!("{v:.1}× real time") }));
            if e.is_gpu() {
                let label = if measured.is_some() && vram.is_none() { "VRAM (ran on CPU)" } else { "VRAM" };
                stat_row(ui, label, vram.map(|v| (v / 8192.0).min(1.0)), vram.map(fmt_mb));
                stat_row(ui, "RAM", ram.map(|v| (v / 16384.0).min(1.0)), ram.map(fmt_mb));
            } else {
                stat_row(ui, "RAM", ram.map(|v| (v / 8192.0).min(1.0)), ram.map(fmt_mb));
            }
            ui.label(RichText::new(src).small().weak());
            let on_cpu = self.loaded.get(&format!("{}:{}", e.id, self.cfg.language)).map(|d| d == "cpu").unwrap_or(false);
            if on_cpu && e.is_gpu() {
                ui.label(RichText::new("No CUDA GPU found, so this engine is running on the CPU.").small().color(GOLD));
            }

            if let Some(inst) = self.installs.get(&e.env) {
                let frac = (inst.mb / e.env_mb as f32).min(0.99);
                ui.add_space(2.0);
                ui.label(RichText::new("Step 1 of 2 · Installing Python packages").small());
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_height(12.0)
                        .text(RichText::new(format!("{} of ~{}", fmt_mb(inst.mb), fmt_mb(e.env_mb as f32))).small()),
                );
                let mut cancel = false;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} · {}", fmt_elapsed(inst.started.elapsed()), truncate(&inst.line, 42))).small().weak());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        cancel = icons::labeled(ui, Icon::Close, "Cancel").clicked();
                    });
                });
                if cancel {
                    engine::cancel_install(&inst.handle);
                }
                return;
            }
            if let Some(done) = self.model_dl.get(&e.id).copied() {
                let frac = (done / e.model_mb as f32).min(0.99);
                ui.add_space(2.0);
                ui.label(RichText::new("Step 2 of 2 · Downloading the model").small());
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_height(12.0)
                        .text(RichText::new(format!("{} of ~{}", fmt_mb(done), fmt_mb(e.model_mb as f32))).small()),
                );
                return;
            }
            ui.horizontal_wrapped(|ui| {
                if !installed {
                    if self.agree_prompt.as_deref() == Some(e.id.as_str()) {
                        ui.label(RichText::new(&e.license).small());
                        if ui.button("I agree, install").clicked() {
                            self.cfg.agreed.push(e.id.clone());
                            self.cfg.save();
                            self.agree_prompt = None;
                            self.start_install(&e.env);
                        }
                        if ui.button("Cancel").clicked() {
                            self.agree_prompt = None;
                        }
                    } else {
                        let r = ui.add_enabled_ui(self.uv.is_some(), |ui| icons::labeled(ui, Icon::Download, "Install")).inner;
                        if r.clicked() {
                            if e.requires_agreement && !self.agreed(&e.id) {
                                self.agree_prompt = Some(e.id.clone());
                            } else {
                                self.start_install(&e.env);
                            }
                        }
                    }
                } else if let Some(msg) = self.benching.get(&e.id) {
                    ui.spinner();
                    ui.label(RichText::new(msg).small());
                } else if speaks_lang {
                    let can = self.cfg.voice_path().is_some();
                    let r = ui.add_enabled_ui(can, |ui| icons::labeled(ui, Icon::Gauge, "Test on my PC")).inner;
                    if r.on_disabled_hover_text("Add a voice clip first").clicked() {
                        self.start_bench(e);
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.hyperlink_to(RichText::new("info").small(), &e.url);
                    let size = if installed {
                        "installed".to_string()
                    } else {
                        let shared = cat.engines.iter().any(|o| o.env == e.env && o.id != e.id && engine::is_installed(&o.env));
                        if shared {
                            format!("~{} model", fmt_mb(e.model_mb as f32))
                        } else {
                            format!("~{} on disk", fmt_mb((e.env_mb + e.model_mb) as f32))
                        }
                    };
                    ui.label(RichText::new(size).small().weak());
                });
            });
        });
        // Bring an engine into view when its install starts.
        if self.installs.contains_key(&e.env) && self.scroll_to.as_deref() == Some(e.env.as_str()) {
            card.response.scroll_to_me(Some(Align::Center));
            self.scroll_to = None;
        }
    }

    fn start_install(&mut self, env: &str) {
        let (Some(uv), Some(dir)) = (self.uv.clone(), self.engines_dir.clone()) else { return };
        let handle = engine::install(uv, dir, env.to_string(), self.tx.clone());
        self.scroll_to = Some(env.to_string());
        self.installs.insert(env.to_string(), Install { started: Instant::now(), mb: 0.0, line: "Starting…".into(), handle });
    }

    fn start_bench(&mut self, e: &Engine) {
        let (Some(uv), Some(dir), Some(voice)) = (self.uv.clone(), self.engines_dir.clone(), self.cfg.voice_path()) else { return };
        self.benching.insert(e.id.clone(), "Starting…".into());
        engine::bench(uv, dir, e.env.clone(), e.id.clone(), voice, self.cfg.language.clone(), self.agreed(&e.id), self.cfg.int8, self.tx.clone());
    }

    fn voices_page(&mut self, ui: &mut egui::Ui) {
        self.back_header(ui, "Voices");
        ui.label(
            RichText::new("Use 10–20 seconds of you talking normally: no music, no background noise. Corvox uses the part that ends at a natural pause.")
                .small()
                .weak(),
        );
        ui.add_space(4.0);
        self.error_line(ui);
        let mut delete = None;
        let mut select = None;
        let mut play = None;
        for v in self.voices.clone() {
            ui.horizontal(|ui| {
                let sel = self.cfg.voice.as_deref() == Some(v.as_str());
                if ui.radio(sel, &v).clicked() && !sel {
                    select = Some(v.clone());
                }
                let secs = audio::wav_seconds(&config::voices_dir().join(format!("{v}.wav"))).unwrap_or(0.0);
                ui.label(RichText::new(format!("{secs:.0} s clip")).small().weak());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icons::button(ui, Icon::Trash, "Delete").clicked() {
                        delete = Some(v.clone());
                    }
                    if icons::button(ui, Icon::Play, "Play the clip").clicked() {
                        play = Some(v.clone());
                    }
                });
            });
        }
        if let Some(v) = select {
            self.cfg.voice = Some(v);
            self.cfg.save();
            self.preload(true);
        }
        if let Some(v) = play {
            if let Ok(r) = hound::WavReader::open(config::voices_dir().join(format!("{v}.wav"))) {
                let sr = r.spec().sample_rate;
                let s: Vec<f32> = r.into_samples::<i16>().flatten().map(|x| x as f32 / 32768.0).collect();
                self.player.stop();
                self.player.push(&s, sr);
            }
        }
        if let Some(v) = delete {
            let _ = std::fs::remove_file(config::voices_dir().join(format!("{v}.wav")));
            self.voices = config::list_voices();
            if self.cfg.voice.as_deref() == Some(v.as_str()) {
                self.cfg.voice = self.voices.first().cloned();
                self.cfg.save();
            }
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if self.importing {
                ui.spinner();
                ui.label("Preparing the clip…");
            } else if icons::labeled(ui, Icon::Plus, "Add voice from a file…").clicked() {
                self.import_voice();
            }
        });
    }

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        self.back_header(ui, "Settings");
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(4.0);
            ui.label(RichText::new("Send the voice to").small().weak());
            let auto = audio::guess_virtual_device(&self.devices);
            let current = self.cfg.output_device.clone();
            let shown = current.clone().unwrap_or_else(|| format!("Auto ({})", auto.clone().unwrap_or_else(|| "no virtual cable found".into())));
            let mut changed = false;
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("dev").selected_text(truncate(&shown, 40)).width(300.0).show_ui(ui, |ui| {
                    if ui.selectable_label(current.is_none(), "Auto").clicked() {
                        self.cfg.output_device = None;
                        changed = true;
                    }
                    for d in self.devices.clone() {
                        if ui.selectable_label(current.as_deref() == Some(d.as_str()), &d).clicked() {
                            self.cfg.output_device = Some(d);
                            changed = true;
                        }
                    }
                });
                if icons::button(ui, Icon::Refresh, "Refresh devices").clicked() {
                    changed = true;
                }
            });
            if !self.player.has_mic() {
                ui.label(
                    RichText::new(if cfg!(windows) {
                        "No virtual cable yet. Install VB-Cable, then pick \"CABLE Output\" as your mic in Discord."
                    } else if cfg!(target_os = "macos") {
                        "No virtual cable yet. Install BlackHole 2ch, then pick it as your mic in Discord."
                    } else {
                        "No virtual cable yet. See the README for the one-line PipeWire/PulseAudio setup."
                    })
                    .small()
                    .color(GOLD),
                );
            }
            if let Some(e) = &self.player.error {
                ui.label(RichText::new(e).small().color(RED));
            }
            ui.add_space(6.0);
            changed |= ui.checkbox(&mut self.cfg.monitor, "Also play it on my speakers/headphones").changed();
            if self.cfg.monitor && ui.add(egui::Slider::new(&mut self.cfg.monitor_volume, 0.0..=1.0).text("volume")).changed() {
                self.player.set_monitor_gain(self.cfg.monitor_volume);
                self.cfg.save();
            }
            if changed {
                self.cfg.save();
                self.rebuild_player();
            }

            ui.add_space(8.0);
            if ui
                .checkbox(&mut self.cfg.int8, "Faster, lighter Pocket engines (int8)")
                .on_hover_text("About 10% faster and ~450 MB less RAM. Re-run Test on my PC to compare.")
                .changed()
            {
                self.cfg.save();
                self.restart_helper();
            }

            ui.add_space(8.0);
            ui.label(RichText::new("Push-to-talk key (held while speaking)").small().weak());
            let mut ptt_changed = false;
            egui::ComboBox::from_id_salt("ptt").selected_text(self.cfg.ptt_key.clone().unwrap_or_else(|| "Off".into())).show_ui(ui, |ui| {
                ptt_changed |= ui.selectable_value(&mut self.cfg.ptt_key, None, "Off").changed();
                for k in ptt::KEYS {
                    ptt_changed |= ui.selectable_value(&mut self.cfg.ptt_key, Some(k.to_string()), k).changed();
                }
            });
            ui.label(RichText::new("Set the same key as Push to Talk in Discord. F13–F24 don't clash with anything.").small().weak());
            if ptt_changed {
                self.cfg.save();
            }

            ui.add_space(8.0);
            ui.label(RichText::new("Shortcut to bring up the window").small().weak());
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.hotkey_text).desired_width(180.0));
                if ui.button("Apply").clicked() {
                    self.cfg.hotkey = self.hotkey_text.trim().to_string();
                    self.cfg.save();
                    self.error = None;
                    self.apply_hotkey();
                }
            });
            self.error_line(ui);

            ui.add_space(8.0);
            ui.label(RichText::new("Hugging Face login (for Kyutai's Pocket TTS voices)").small().weak());
            if config::has_hf_token() {
                ui.horizontal(|ui| {
                    icons::small(ui, Icon::Check, GREEN);
                    ui.label(RichText::new("Token saved").small().color(GREEN));
                });
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("1.").small());
                ui.hyperlink_to(RichText::new("Accept the terms on kyutai/pocket-tts").small(), "https://huggingface.co/kyutai/pocket-tts");
                ui.label(RichText::new("2.").small());
                ui.hyperlink_to(RichText::new("Create a read token").small(), "https://huggingface.co/settings/tokens");
            });
            let mut save = false;
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.hf_input).password(true).hint_text("hf_…").desired_width(220.0));
                save = ui.button("Save token").clicked() && !self.hf_input.trim().is_empty();
            });
            if save {
                self.hf_msg = Some(match config::save_hf_token(&self.hf_input) {
                    Ok(()) => {
                        self.hf_input.clear();
                        self.restart_helper(); // reload models with the cloning weights
                        "Saved. Restarting the engine.".into()
                    }
                    Err(e) => format!("Couldn't save: {e}"),
                });
            }
            if let Some(m) = &self.hf_msg {
                ui.label(RichText::new(m).small().weak());
            }

            ui.add_space(8.0);
            ui.label(RichText::new("Paths").small().weak());
            let eng = self.engines_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "not found".into());
            let uv = self.uv.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "not installed".into());
            ui.label(RichText::new(format!("Engines: {eng}")).small());
            ui.label(RichText::new(format!("uv: {uv}")).small());
            ui.label(RichText::new(format!("Data: {}", config::data_dir().display())).small());
        });
    }

    fn pill(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::new(20.0, 28.0), Sense::hover());
            let handle = ui.interact(r, ui.scope_id().with("pill-drag"), Sense::click_and_drag()).on_hover_text("Drag to move");
            if handle.drag_started() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            let tint = if self.busy() { AMBER } else { ui.visuals().weak_text_color() };
            ui.put(r, Icon::Mic.image(16.0, tint));
            let tag = format!("{} · {}", self.cfg.language.to_uppercase(), self.engine().map(|e| short_name(&e.name)).unwrap_or_default());
            let w = ui.available_width() - 74.0 - tag.chars().count() as f32 * 6.5;
            self.text_box(ui, w.max(120.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.busy() {
                    if icons::button_tinted(ui, Icon::Stop, AMBER, "Stop (Esc)").clicked() {
                        self.stop();
                    }
                } else if icons::button(ui, Icon::Send, "Speak (Enter)").clicked() {
                    let t = std::mem::take(&mut self.text);
                    self.speak(t);
                    self.focus_text = true;
                }
                if icons::button(ui, Icon::ChevronUp, "Expand").clicked() {
                    self.expand(true);
                }
                let col = if self.error.is_some() { RED } else { ui.visuals().weak_text_color() };
                let dl = self.engine().and_then(|e| self.model_dl.get(&e.id).map(|d| pct_or_mb(*d, e.model_mb)));
                let label = match dl {
                    Some(d) => format!("Downloading {d}"),
                    None => tag,
                };
                let r = ui.label(RichText::new(label).small().color(col));
                if let Some(e) = &self.error {
                    r.on_hover_text(e);
                }
            });
        });
    }

    /// Debug aid: `CORVOX_SHOT=out.png` saves a screenshot after a moment, then quits.
    fn debug_screenshot(&mut self, ctx: &egui::Context) {
        let Ok(path) = std::env::var("CORVOX_SHOT") else { return };
        self.frames += 1;
        let waiting = std::env::var("CORVOX_SAY").is_ok() && (self.busy() || self.queued.is_some() || !self.helper_ready);
        if waiting && self.frames < 2400 {
            self.frames = self.frames.min(38);
        }
        let wait_frames = std::env::var("CORVOX_SHOT_AFTER").ok().and_then(|s| s.parse::<u32>().ok()).map(|secs| secs * 20).unwrap_or(40);
        if self.frames < wait_frames {
            ctx.request_repaint_after(Duration::from_millis(50));
        } else if self.frames == wait_frames {
            let c = ctx.clone();
            ctx.request_screenshot(move |img| {
                let [w, h] = img.size;
                let _ = image::save_buffer(&path, img.as_raw(), w as u32, h as u32, image::ExtendedColorType::Rgba8);
                c.send_viewport_cmd(egui::ViewportCommand::Close);
                c.request_repaint();
            });
            ctx.request_repaint();
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_events();
        self.update_ptt();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.debug_screenshot(ui.ctx());
        let active = self.busy()
            || !self.installs.is_empty()
            || !self.benching.is_empty()
            || self.ptt.is_held()
            || !self.model_dl.is_empty()
            || self.loading.is_some();
        if active || (!self.helper_ready && self.helper.is_some()) {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        let visuals = ui.visuals().clone();
        let collapsed = self.cfg.collapsed;
        let frame = egui::Frame::new()
            .fill(visuals.panel_fill)
            .stroke(visuals.widgets.noninteractive.bg_stroke)
            .corner_radius(CornerRadius::same(if platform::native_corners() { 0 } else if collapsed { 28 } else { 10 }))
            .inner_margin(if collapsed { egui::Margin::symmetric(14, 8) } else { egui::Margin::same(10) });
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            if collapsed {
                self.pill(ui);
                return;
            }
            self.title_bar(ui);
            ui.add_space(4.0);
            match self.page {
                Page::Main => self.main_page(ui),
                Page::Engines => self.engines_page(ui),
                Page::Voices => self.voices_page(ui),
                Page::Settings => self.settings_page(ui),
            }
        });
    }

    fn on_exit(&mut self) {
        self.ptt.release();
        self.cfg.save();
        for i in self.installs.values() {
            engine::cancel_install(&i.handle);
        }
        self.helper = None;
    }
}

// ---------- small widgets ----------

fn style(ctx: &egui::Context) {
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = Vec2::new(6.0, 5.0);
        s.spacing.button_padding = Vec2::new(7.0, 3.0);
        // Solid scroll bars take their own space instead of floating over the content.
        s.spacing.scroll = egui::style::ScrollStyle::solid();
    });
}

fn stat_row(ui: &mut egui::Ui, label: &str, frac: Option<f32>, text: Option<String>) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(Vec2::new(96.0, 14.0), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_width(96.0);
            ui.label(RichText::new(label).small().weak());
        });
        let bar_w = (ui.available_width() - 104.0).max(40.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(bar_w, 5.0), Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::same(3), ui.visuals().extreme_bg_color);
        if let Some(f) = frac {
            let mut r = rect;
            r.set_width(rect.width() * f.clamp(0.0, 1.0));
            p.rect_filled(r, CornerRadius::same(3), ui.visuals().selection.stroke.color);
        }
        ui.label(RichText::new(text.unwrap_or_else(|| "—".into())).small());
    });
}

fn fmt_mb(mb: f32) -> String {
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{mb:.0} MB")
    }
}

fn pct_or_mb(done: f32, total: u32) -> String {
    if total > 0 {
        format!("{:.0}%", (100.0 * done / total as f32).min(99.0))
    } else {
        fmt_mb(done)
    }
}

fn fmt_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

fn short_name(name: &str) -> String {
    name.replace("Pocket TTS Turkish", "Pocket TR")
        .replace("Pocket TTS Large", "Pocket L")
        .replace("Pocket TTS", "Pocket")
        .replace(" Multilingual", "")
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}
