use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver};
use eframe::egui::{self, Align, Color32, CornerRadius, FontId, Key, Layout, RichText, Sense, Stroke, UiBuilder, Vec2};

use crate::audio::{self, Player};
use crate::catalog::{find_engines_dir, find_uv, Catalog, Engine};
use crate::config::{self, Config, Corner};
use crate::engine::{self, CancelHandle, Event, Helper, Tx};
use crate::i18n::{self, t, tf};
use crate::icons::{self, Icon};
use crate::platform;
use crate::ptt::{self, Ptt};
use crate::update;
use crate::vmic;

pub const APP_NAME: &str = "Corvox";
pub const EXPANDED: Vec2 = Vec2::new(440.0, 400.0);
pub const COLLAPSED: Vec2 = Vec2::new(440.0, 56.0);

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

/// A message kept as its English key plus arguments, so it's translated when shown and
/// follows the interface language even after it was created.
#[derive(Clone, PartialEq)]
struct Msg {
    key: &'static str,
    args: Vec<String>,
    from_engine: bool,
}

impl Msg {
    fn new(key: &'static str, args: &[&str]) -> Self {
        Self { key, args: args.iter().map(|a| a.to_string()).collect(), from_engine: false }
    }
    /// Text that can't be translated (errors from the engine, file system, ...).
    fn raw(text: impl Into<String>) -> Self {
        Self { key: "{}", args: vec![text.into()], from_engine: false }
    }
    /// A progress message from the Python helper (known ones get translated).
    fn engine(text: impl Into<String>) -> Self {
        Self { key: "{}", args: vec![text.into()], from_engine: true }
    }
    fn text(&self) -> String {
        if self.from_engine {
            return i18n::engine_status(&self.args[0]);
        }
        let args: Vec<&str> = self.args.iter().map(|a| a.as_str()).collect();
        tf(self.key, &args)
    }
}

/// Where the update check / install stands.
#[derive(Clone, PartialEq)]
enum Upd {
    Idle,
    Checking,
    UpToDate,
    Available(update::Release),
    Downloading { version: String, done: u64, total: u64 },
    Restarting,
    Failed(Msg),
}

#[derive(Clone, PartialEq)]
enum Status {
    Ready,
    StartingEngine,
    Loading,
    Generating,
    Speaking,
    SpeakingSaved,
    Msg(Msg),
}

impl Status {
    fn text(&self) -> String {
        match self {
            Status::Ready => t("Ready").into(),
            Status::StartingEngine => t("Starting engine…").into(),
            Status::Loading => t("Loading the model…").into(),
            Status::Generating => t("Generating…").into(),
            Status::Speaking => t("Speaking").into(),
            Status::SpeakingSaved => t("Speaking (saved audio)").into(),
            Status::Msg(m) => m.text(),
        }
    }
}

struct Install {
    started: Instant,
    mb: f32,
    line: String,
    handle: CancelHandle,
}

/// One helper process. "main" speaks; "dl:<model key>" downloads a model in the background.
struct Hs {
    h: Helper,
    ready: bool,
    /// Loaded models on this helper: model key -> device ("cpu"/"cuda").
    loaded: HashMap<String, String>,
    loading: Option<String>,
    /// Loaded model keys, oldest first (up to two stay loaded).
    order: Vec<String>,
    /// When this helper became the spare (the previous engine, kept warm for quick switches).
    idle_since: Option<Instant>,
    models_freed: bool,
}

/// The previous engine stays loaded this long after a switch, then frees its model...
const SPARE_MODEL_SECS: u64 = 180;
/// ...and its process this long after the switch.
const SPARE_PROCESS_SECS: u64 = 600;

/// A background model download: which engine/language it is for.
#[derive(Clone)]
struct Download {
    engine: String,
    lang: String,
    key: String,
}

/// Audio of the current request: waits for the prebuffer, and is kept whole for the cache.
struct Pending {
    req: u64,
    text: String,
    chunks: Vec<(Vec<f32>, u32)>,
    secs: f32,
    playing: bool,
    all: Vec<f32>,
    sr: u32,
}

pub struct App {
    cfg: Config,
    catalog: Option<Catalog>,
    engines_dir: Option<PathBuf>,
    uv: Option<PathBuf>,
    rx: Receiver<Event>,
    tx: Tx,

    helpers: HashMap<String, Hs>,
    /// Key of the helper that speaks. Every helper gets a fresh key, so a late "exited"
    /// event from a replaced helper can't be mistaken for the new one.
    main_key: String,
    /// The previously used engine's helper, kept running so switching back is instant.
    spare_key: Option<String>,
    helper_gen: u64,
    downloads: HashMap<String, Download>,
    paused_downloads: HashMap<String, f32>,
    /// Model download progress (MB) by model key, from any helper.
    model_dl: HashMap<String, f32>,
    /// Engine to switch to as soon as its model finishes downloading.
    switch_to: Option<String>,
    queued: Option<String>,
    current_req: Option<u64>,
    pending: Option<Pending>,
    cached: HashSet<String>,

    player: Player,
    devices: Vec<String>,
    ptt: Ptt,
    quiet_since: Option<Instant>,

    page: Page,
    text: String,
    history_pos: Option<usize>,
    focus_text: bool,
    status: Status,
    error: Option<Msg>,
    show_all_engines: bool,
    installs: HashMap<String, Install>,
    paused_installs: HashMap<String, f32>,
    vmic_step: Option<String>,
    vmic_msg: Option<(bool, Msg)>,
    benching: HashMap<String, String>,
    agree_prompt: Option<String>,
    delete_prompt: Option<String>,
    deleting: HashSet<String>,
    cache_msg: Option<Msg>,
    cleaning_cache: bool,
    voices: Vec<String>,
    importing: bool,
    hotkey_text: String,
    hf_input: String,
    hf_msg: Option<Msg>,
    hotkey_mgr: Option<global_hotkey::GlobalHotKeyManager>,
    hotkey_cur: Option<global_hotkey::hotkey::HotKey>,
    frames: u32,
    win: Option<platform::Window>,
    prev_window: Option<std::sync::Arc<std::sync::atomic::AtomicIsize>>,
    scroll_to: Option<String>,
    /// Re-apply the corner snap once the first frame knows the screen scale.
    snap_pending: bool,
    upd: Upd,
    /// Running from a release folder we can write to, so updates can install themselves.
    self_update_ok: bool,
    /// Installed engine environments whose packages are being brought up to date after an
    /// app update.
    resyncing: HashSet<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, cfg: Config) -> Self {
        let ctx = cc.egui_ctx.clone();
        egui_extras::install_image_loaders(&ctx);
        style(&ctx);
        i18n::set_lang(&cfg.ui_lang);
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
            helpers: HashMap::new(),
            main_key: String::new(),
            spare_key: None,
            helper_gen: 0,
            downloads: HashMap::new(),
            paused_downloads: HashMap::new(),
            model_dl: HashMap::new(),
            switch_to: None,
            queued: None,
            current_req: None,
            pending: None,
            cached: HashSet::new(),
            player,
            devices,
            ptt: Ptt::new(),
            quiet_since: None,
            page: Page::Main,
            text: String::new(),
            history_pos: None,
            focus_text: true,
            status: Status::Ready,
            error: None,
            show_all_engines: false,
            installs: HashMap::new(),
            paused_installs: HashMap::new(),
            vmic_step: None,
            vmic_msg: None,
            benching: HashMap::new(),
            agree_prompt: None,
            delete_prompt: None,
            deleting: HashSet::new(),
            cache_msg: None,
            cleaning_cache: false,
            voices: config::list_voices(),
            importing: false,
            hf_input: String::new(),
            hf_msg: None,
            hotkey_mgr: global_hotkey::GlobalHotKeyManager::new().ok(),
            hotkey_cur: None,
            frames: 0,
            win: platform::Window::from(cc),
            prev_window: None,
            scroll_to: None,
            snap_pending: true,
            upd: Upd::Idle,
            self_update_ok: update::can_self_update(),
            resyncing: HashSet::new(),
        };
        if app.cfg.voice.as_ref().map(|v| !app.voices.contains(v)).unwrap_or(true) {
            app.cfg.voice = app.voices.first().cloned();
        }
        if let Some(w) = app.win {
            w.round_corners();
            app.prev_window = Some(w.track_previous());
        }
        if app.cfg.vmic_setup {
            vmic::restore_on_startup(app.tx.clone());
        }
        app.tidy_phrase_cache();
        app.apply_hotkey();
        app.fix_engine_for_language();
        app.resync_outdated();
        app.ensure_main();
        if app.cfg.check_updates && std::env::var_os("CORVOX_SHOT").is_none() {
            update::check(app.tx.clone(), false);
        }
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

    fn engine_by_id(&self, id: &str) -> Option<Engine> {
        self.catalog.as_ref()?.engine(id).cloned()
    }

    fn agreed(&self, id: &str) -> bool {
        self.cfg.agreed.iter().any(|a| a == id)
    }

    fn busy(&self) -> bool {
        self.current_req.is_some() || self.player.is_playing() || self.pending.is_some()
    }

    fn model_key_for(&self, e: &Engine) -> String {
        e.model_key(e.load_language(&self.cfg.language))
    }

    fn model_ready(&self, e: &Engine) -> bool {
        engine::is_installed(&e.env) && self.cfg.is_downloaded(&self.model_key_for(e))
    }

    fn apply_hotkey(&mut self) {
        let Some(mgr) = &self.hotkey_mgr else { return };
        if let Some(old) = self.hotkey_cur.take() {
            let _ = mgr.unregister(old);
        }
        match self.cfg.hotkey.parse::<global_hotkey::hotkey::HotKey>() {
            Ok(hk) => match mgr.register(hk) {
                Ok(()) => self.hotkey_cur = Some(hk),
                Err(e) => self.error = Some(Msg::new("Couldn't register {}: {}", &[&self.cfg.hotkey, &e.to_string()])),
            },
            Err(e) => self.error = Some(Msg::new("Bad shortcut \"{}\": {}", &[&self.cfg.hotkey, &e.to_string()])),
        }
    }

    fn fix_engine_for_language(&mut self) {
        let Some(cat) = self.catalog.clone() else { return };
        let ok = cat.engine(&self.cfg.engine).map(|e| e.languages.contains(&self.cfg.language)).unwrap_or(false);
        if !ok {
            let list = cat.engines_for(&self.cfg.language);
            let pick = list
                .iter()
                .find(|e| self.model_ready(e))
                .or_else(|| list.iter().find(|e| engine::is_installed(&e.env)))
                .or(list.first());
            if let Some(e) = pick {
                self.cfg.engine = e.id.clone();
            }
        }
    }

    fn spawn_helper(&mut self, key: &str, e: &Engine) -> bool {
        let (Some(uv), Some(dir)) = (self.uv.clone(), self.engines_dir.clone()) else { return false };
        match Helper::spawn(&uv, &dir, &e.env, key, self.agreed(&e.id), self.cfg.int8, self.tx.clone()) {
            Ok(h) => {
                self.helpers.insert(
                    key.to_string(),
                    Hs { h, ready: false, loaded: HashMap::new(), loading: None, order: vec![], idle_since: None, models_freed: false },
                );
                true
            }
            Err(err) => {
                self.error = Some(Msg::new("Couldn't start engine: {}", &[&err.to_string()]));
                false
            }
        }
    }

    /// Makes sure the "main" helper runs the selected engine's environment, then loads its model.
    fn ensure_main(&mut self) {
        let Some(e) = self.engine().cloned() else { return };
        if !engine::is_installed(&e.env) {
            self.helpers.remove(&self.main_key);
            return;
        }
        if self.resyncing.contains(&e.env) {
            return; // its packages are being updated; it starts when that's done
        }
        let alive = self.helpers.get_mut(&self.main_key).map(|hs| hs.h.env == e.env && hs.h.is_alive()).unwrap_or(false);
        if alive {
            if self.helpers.get(&self.main_key).map(|hs| hs.ready).unwrap_or(false) {
                self.preload(false);
            }
            return;
        }
        // Switching back to the previous engine: its helper is still running (and usually
        // still has the model loaded), so just swap the two.
        if let Some(sk) = self.spare_key.clone() {
            let spare_ok = self.helpers.get_mut(&sk).map(|hs| hs.h.env == e.env && hs.h.is_alive()).unwrap_or(false);
            if spare_ok {
                let old = std::mem::replace(&mut self.main_key, sk.clone());
                self.spare_key = None;
                if let Some(hs) = self.helpers.get_mut(&sk) {
                    hs.idle_since = None;
                    hs.models_freed = false;
                }
                self.park_as_spare(old);
                if self.helpers.get(&self.main_key).map(|hs| hs.ready).unwrap_or(false) {
                    self.preload(false);
                } else {
                    self.status = Status::StartingEngine;
                }
                return;
            }
        }
        let old = std::mem::take(&mut self.main_key);
        self.park_as_spare(old);
        let key = self.next_key("main");
        if self.spawn_helper(&key, &e) {
            self.main_key = key;
            self.status = Status::StartingEngine;
        }
    }

    /// Keeps a helper we're switching away from running as the spare (replacing any older one).
    fn park_as_spare(&mut self, key: String) {
        if key.is_empty() || !self.helpers.contains_key(&key) {
            return;
        }
        if let Some(old) = self.spare_key.take() {
            self.helpers.remove(&old);
        }
        if let Some(hs) = self.helpers.get_mut(&key) {
            hs.idle_since = Some(Instant::now());
            hs.models_freed = false;
        }
        self.spare_key = Some(key);
    }

    /// Frees the spare's model after a while, then the spare itself.
    fn tend_spare(&mut self) {
        let Some(sk) = self.spare_key.clone() else { return };
        let Some(hs) = self.helpers.get_mut(&sk) else {
            self.spare_key = None;
            return;
        };
        let idle = hs.idle_since.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        if idle >= SPARE_PROCESS_SECS {
            self.helpers.remove(&sk);
            self.spare_key = None;
        } else if idle >= SPARE_MODEL_SECS && !hs.models_freed {
            let ids: HashSet<String> = hs.loaded.keys().map(|k| k.split(':').next().unwrap_or(k).to_string()).collect();
            for id in ids {
                hs.h.unload(&id);
            }
            hs.loaded.clear();
            hs.order.clear();
            hs.models_freed = true;
        }
    }

    fn drop_spare(&mut self) {
        if let Some(sk) = self.spare_key.take() {
            self.helpers.remove(&sk);
        }
    }

    fn next_key(&mut self, prefix: &str) -> String {
        self.helper_gen += 1;
        format!("{prefix}#{}", self.helper_gen)
    }

    /// The background-download helper for a model, if one is running.
    fn download_helper(&self, model_key: &str) -> Option<String> {
        self.downloads.iter().find(|(_, d)| d.key == model_key).map(|(k, _)| k.clone())
    }

    fn restart_main(&mut self) {
        self.drop_spare();
        self.helpers.remove(&self.main_key);
        self.main_key.clear();
        self.ensure_main();
    }

    /// Loads the selected model on the main helper and prepares the voice. Models that aren't
    /// downloaded yet go to a background download instead, so speaking never blocks on one.
    fn preload(&mut self, force: bool) {
        let Some(e) = self.engine().cloned() else { return };
        let lang = e.load_language(&self.cfg.language).to_string();
        let key = e.model_key(&lang);
        if !self.cfg.is_downloaded(&key) {
            self.switch_to = Some(e.id.clone());
            self.start_download(&e);
            return;
        }
        let voice = self.cfg.voice_path();
        let Some(hs) = self.helpers.get_mut(&self.main_key) else { return };
        if !force && (hs.loaded.contains_key(&key) || hs.loading.as_deref() == Some(key.as_str())) {
            hs.order.retain(|k| *k != key);
            hs.order.push(key.clone());
            return;
        }
        // Keep the two most recent models loaded so switching between them is instant.
        if !hs.loaded.contains_key(&key) && hs.loaded.len() >= 2 {
            if let Some(oldest) = hs.order.iter().find(|k| **k != key).cloned() {
                let id = oldest.split(':').next().unwrap_or(&oldest).to_string();
                hs.h.unload(&id);
                let prefix = format!("{id}:");
                hs.loaded.retain(|k, _| *k != id && !k.starts_with(&prefix));
                hs.order.retain(|k| *k != id && !k.starts_with(&prefix));
            }
        }
        if hs.h.load(&e.id, &lang, voice.as_deref(), e.model_mb).is_ok() {
            hs.loading = Some(key);
            self.status = Status::Loading;
        }
    }

    // ---------- background model downloads ----------

    fn start_download(&mut self, e: &Engine) {
        if !engine::is_installed(&e.env) {
            return;
        }
        let lang = e.load_language(&self.cfg.language).to_string();
        let key = e.model_key(&lang);
        if self.download_helper(&key).is_some() {
            return;
        }
        let hkey = self.next_key(&format!("dl:{key}"));
        self.paused_downloads.remove(&key);
        if self.spawn_helper(&hkey, e) {
            self.downloads.insert(hkey, Download { engine: e.id.clone(), lang, key: key.clone() });
            self.model_dl.entry(key).or_insert(0.0);
        }
    }

    fn pause_download(&mut self, key: &str) {
        if let Some(hkey) = self.download_helper(key) {
            self.helpers.remove(&hkey); // stopping the helper stops the download; resuming continues it
            self.downloads.remove(&hkey);
        }
        let done = self.model_dl.remove(key).unwrap_or(0.0);
        self.paused_downloads.insert(key.to_string(), done);
        if self.switch_to.as_deref().and_then(|id| self.engine_by_id(id)).map(|e| self.model_key_for(&e) == key).unwrap_or(false) {
            self.switch_to = None;
        }
    }

    fn finish_download(&mut self, hkey: &str, result: Result<String, Msg>) {
        self.helpers.remove(hkey);
        let Some(dl) = self.downloads.remove(hkey) else { return };
        self.model_dl.remove(&dl.key);
        match result {
            Ok(_) => {
                self.cfg.mark_downloaded(&dl.key);
                self.cfg.save();
                if self.switch_to.as_deref() == Some(dl.engine.as_str()) {
                    self.switch_to = None;
                    self.cfg.engine = dl.engine.clone();
                    if let Some(e) = self.engine_by_id(&dl.engine) {
                        if e.languages.contains(&dl.lang) {
                            self.cfg.language = dl.lang.clone();
                        }
                    }
                    self.cfg.save();
                    self.ensure_main();
                }
                let name = self.engine_by_id(&dl.engine).map(|e| e.name).unwrap_or_default();
                self.status = Status::Msg(Msg::new("{} is ready", &[&name]));
                let (chime, sr) = audio::chime();
                self.player.play_local(&chime, sr);
            }
            Err(err) => {
                self.paused_downloads.insert(dl.key, 0.0);
                self.error = Some(err);
            }
        }
    }

    /// Picks an engine from its card. If its model isn't downloaded yet, downloads it in the
    /// background and switches when it's done; the current engine keeps working meanwhile.
    fn select_engine(&mut self, e: &Engine) {
        if self.model_ready(e) {
            self.switch_to = None;
            self.cfg.engine = e.id.clone();
            self.cfg.save();
            self.ensure_main();
        } else {
            self.switch_to = Some(e.id.clone());
            self.start_download(e);
        }
    }

    // ---------- deleting engines ----------

    /// Has anything of this engine landed on disk (packages, model, or a partial download)?
    fn has_files(&self, e: &Engine) -> bool {
        engine::is_installed(&e.env)
            || config::envs_dir().join(&e.env).exists()
            || e.hf_repos.iter().any(|r| config::hf_hub_dir().join(format!("models--{}", r.replace('/', "--"))).exists())
    }

    fn delete_engine(&mut self, e: &Engine) {
        let Some(cat) = self.catalog.clone() else { return };
        let same_env: Vec<Engine> = cat.engines.iter().filter(|o| o.env == e.env).cloned().collect();
        // Stop everything that uses these files first.
        if self.helpers.get(&self.main_key).map(|h| h.h.env == e.env).unwrap_or(false) {
            self.helpers.remove(&self.main_key);
        }
        if let Some(sk) = self.spare_key.clone() {
            if self.helpers.get(&sk).map(|h| h.h.env == e.env).unwrap_or(true) {
                self.drop_spare();
            }
        }
        let dl_keys: Vec<String> = self
            .downloads
            .iter()
            .filter(|(_, d)| same_env.iter().any(|o| o.id == d.engine))
            .map(|(k, _)| k.clone())
            .collect();
        for k in dl_keys {
            if let Some(d) = self.downloads.remove(&k) {
                self.model_dl.remove(&d.key);
            }
            self.helpers.remove(&k);
        }
        if let Some(inst) = self.installs.remove(&e.env) {
            engine::cancel_install(&inst.handle);
        }
        self.paused_installs.remove(&e.env);
        // Models: this engine's repos, which may be shared (Pocket and Pocket Large).
        let repos = e.hf_repos.clone();
        let affected: Vec<Engine> = cat.engines.iter().filter(|o| o.hf_repos.iter().any(|r| repos.contains(r)) || o.env == e.env).cloned().collect();
        for o in &affected {
            let id = o.id.clone();
            self.cfg.forget_downloaded(|k| k == id || k.starts_with(&format!("{id}:")));
            self.paused_downloads.retain(|k, _| !(k == &id || k.starts_with(&format!("{id}:"))));
        }
        if self.switch_to.as_deref().map(|s| affected.iter().any(|o| o.id == s)).unwrap_or(false) {
            self.switch_to = None;
        }
        self.cfg.save();
        self.deleting.insert(e.id.clone());
        engine::delete_engine_files(e.id.clone(), Some(e.env.clone()), repos, self.tx.clone());
    }

    // ---------- phrase audio cache ----------

    fn phrase_key(&self, text: &str) -> Option<String> {
        let e = self.engine()?;
        let voice = self.cfg.voice.as_ref()?;
        let vp = self.cfg.voice_path()?;
        let stamp = std::fs::metadata(&vp)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let int8 = e.id.starts_with("pocket") && self.cfg.int8;
        Some(format!("{:016x}", fnv(&format!("{}|{}|{}|{}|{}|{}", e.id, self.cfg.language, voice, stamp, int8, text.trim()))))
    }

    fn phrase_path(key: &str) -> PathBuf {
        config::phrase_cache_dir().join(format!("{key}.wav"))
    }

    /// Keeps only audio for the phrases still listed (with the current engine and voice).
    fn tidy_phrase_cache(&mut self) {
        let keep: HashSet<String> = if self.cfg.keep_phrases {
            self.phrase_list().iter().filter_map(|p| self.phrase_key(p)).collect()
        } else {
            HashSet::new()
        };
        self.cached.clear();
        for entry in std::fs::read_dir(config::phrase_cache_dir()).into_iter().flatten().flatten() {
            let p = entry.path();
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()).map(String::from) else { continue };
            if keep.contains(&stem) {
                self.cached.insert(stem);
            } else {
                let _ = std::fs::remove_file(&p);
            }
        }
    }

    fn forget_phrases(&mut self, removed: &[String]) {
        for p in removed {
            if let Some(k) = self.phrase_key(p) {
                let _ = std::fs::remove_file(Self::phrase_path(&k));
                self.cached.remove(&k);
            }
        }
    }

    fn play_saved(&mut self, key: &str) -> bool {
        let Ok(r) = hound::WavReader::open(Self::phrase_path(key)) else {
            self.cached.remove(key);
            return false;
        };
        let sr = r.spec().sample_rate;
        let s: Vec<f32> = r.into_samples::<i16>().flatten().map(|x| x as f32 / 32768.0).collect();
        if s.is_empty() {
            return false;
        }
        self.play_now(vec![(s, sr)]);
        self.status = Status::SpeakingSaved;
        true
    }

    fn save_phrase_audio(&mut self, text: &str, samples: Vec<f32>, sr: u32) {
        if !self.cfg.keep_phrases || samples.is_empty() || !self.phrase_list().iter().any(|p| p == text) {
            return;
        }
        let Some(key) = self.phrase_key(text) else { return };
        self.cached.insert(key.clone());
        std::thread::spawn(move || {
            let _ = audio::write_wav(&Self::phrase_path(&key), &samples, sr);
        });
    }

    // ---------- speaking ----------

    fn speak(&mut self, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.error = None;
        let removed = self.cfg.push_recent(&text);
        self.forget_phrases(&removed);
        self.cfg.save();
        if self.busy() {
            self.stop(); // a new line interrupts the old one
        }
        if self.cfg.voice_path().is_none() {
            self.error = Some(Msg::new("Add your voice clip first.", &[]));
            self.page = Page::Voices;
            self.expand(true);
            return;
        }
        // Saved audio plays instantly, even while engines are busy or downloading.
        if self.cfg.keep_phrases {
            if let Some(key) = self.phrase_key(&text) {
                if self.cached.contains(&key) && self.play_saved(&key) {
                    return;
                }
            }
        }
        let Some(e) = self.engine().cloned() else { return };
        if !engine::is_installed(&e.env) {
            self.error = Some(Msg::new("Install {} first.", &[&e.name]));
            self.page = Page::Engines;
            self.expand(true);
            return;
        }
        if !self.cfg.is_downloaded(&self.model_key_for(&e)) {
            self.preload(false);
            self.error = Some(Msg::new("{} is still downloading. Pick another engine meanwhile.", &[&e.name]));
            return;
        }
        self.ensure_main();
        let ready = self.helpers.get(&self.main_key).map(|hs| hs.ready).unwrap_or(false);
        if !ready {
            self.queued = Some(text);
            return;
        }
        let voice = self.cfg.voice_path().unwrap_or_default();
        let lang = self.cfg.language.clone();
        let Some(hs) = self.helpers.get_mut(&self.main_key) else { return };
        match hs.h.speak(&e.id, &text, &voice, &lang, e.model_mb) {
            Ok(id) => {
                self.current_req = Some(id);
                self.pending = Some(Pending { req: id, text, chunks: vec![], secs: 0.0, playing: false, all: vec![], sr: 24000 });
                self.status = Status::Generating;
            }
            Err(err) => self.error = Some(Msg::new("Engine error: {}", &[&err.to_string()])),
        }
    }

    fn stop(&mut self) {
        if self.current_req.is_some() {
            if let Some(hs) = self.helpers.get_mut(&self.main_key) {
                hs.h.cancel();
            }
        }
        self.current_req = None;
        self.pending = None;
        self.queued = None;
        self.player.stop();
        self.ptt.release();
        self.status = Status::Ready;
    }

    fn play_now(&mut self, chunks: Vec<(Vec<f32>, u32)>) {
        if self.cfg.ptt_key.is_some() && !self.ptt.is_held() {
            self.ptt.press(self.cfg.ptt_key.as_deref());
            self.player.push_silence(0.15); // give Discord a moment to open the mic
        }
        for (s, sr) in chunks {
            self.player.push(&s, sr);
        }
        self.status = Status::Speaking;
    }

    fn rebuild_player(&mut self) {
        self.devices = audio::output_device_names();
        let mic = self.cfg.output_device.clone().or_else(|| audio::guess_virtual_device(&self.devices));
        self.player = Player::new(mic.as_deref(), self.cfg.monitor, self.cfg.monitor_volume);
    }

    // ---------- window ----------

    fn expand(&mut self, expanded: bool) {
        if self.cfg.collapsed == !expanded {
            return;
        }
        self.cfg.collapsed = !expanded;
        self.cfg.save();
        let size = if expanded { EXPANDED } else { COLLAPSED };
        self.tx.1.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        if let Some(c) = self.cfg.snap {
            self.snap_to(c, size); // grow/shrink in place, staying in the corner
        }
        self.focus_text = true;
    }

    /// Moves the window into a corner of its screen's usable area (taskbar excluded).
    fn snap_to(&mut self, corner: Corner, size: Vec2) {
        let ctx = self.tx.1.clone();
        let ppp = ctx.pixels_per_point();
        let area = self.win.and_then(|w| w.work_area()).map(|(l, t, r, b)| (l as f32, t as f32, r as f32, b as f32)).or_else(|| {
            ctx.input(|i| i.viewport().monitor_size).map(|m| (0.0, 0.0, m.x * ppp, m.y * ppp))
        });
        let Some((l, t, r, b)) = area else { return };
        let margin = 12.0 * ppp;
        let (w, h) = (size.x * ppp, size.y * ppp);
        let x = if corner.left() { l + margin } else { r - w - margin };
        let y = if corner.top() { t + margin } else { b - h - margin };
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x / ppp, y / ppp)));
    }

    fn set_snap(&mut self, corner: Option<Corner>) {
        self.cfg.snap = corner;
        self.cfg.save();
        if let Some(c) = corner {
            let size = if self.cfg.collapsed { COLLAPSED } else { EXPANDED };
            self.snap_to(c, size);
        }
    }

    /// Four arrows from the center to the corners, their 90° tips outlining a square.
    /// Clicking the arrow toward a corner sticks the window there; clicking it again unsticks.
    fn snap_widget(&mut self, ui: &mut egui::Ui) {
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(t("Stick to a corner: click the arrow toward it. Click it again to unstick."));
        let c = rect.center();
        let corner_at = |p: egui::Pos2| match (p.x < c.x, p.y < c.y) {
            (true, true) => Corner::TopLeft,
            (false, true) => Corner::TopRight,
            (true, false) => Corner::BottomLeft,
            (false, false) => Corner::BottomRight,
        };
        let hovered = resp.hover_pos().map(corner_at);
        let accent = ui.visuals().selection.stroke.color;
        let normal = ui.visuals().widgets.inactive.fg_stroke.color;
        let strong = ui.visuals().strong_text_color();
        let painter = ui.painter_at(rect);
        let reach = 8.0;
        let leg = 4.5;
        for corner in Corner::ALL {
            let sx = if corner.left() { -1.0 } else { 1.0 };
            let sy = if corner.top() { -1.0 } else { 1.0 };
            let (color, width) = if self.cfg.snap == Some(corner) {
                (accent, 2.0)
            } else if hovered == Some(corner) {
                (strong, 1.7)
            } else {
                (normal, 1.4)
            };
            let stroke = Stroke::new(width, color);
            let tip = c + Vec2::new(sx * reach, sy * reach);
            painter.line_segment([c + Vec2::new(sx * 2.0, sy * 2.0), tip], stroke);
            painter.line_segment([tip, tip - Vec2::new(sx * leg, 0.0)], stroke);
            painter.line_segment([tip, tip - Vec2::new(0.0, sy * leg)], stroke);
        }
        if resp.clicked() {
            if let Some(corner) = resp.interact_pointer_pos().map(corner_at) {
                let next = if self.cfg.snap == Some(corner) { None } else { Some(corner) };
                self.set_snap(next);
            }
        }
    }

    fn set_on_top(&mut self, on: bool) {
        self.cfg.always_on_top = on;
        self.cfg.save();
        let lvl = if on { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
        self.tx.1.send_viewport_cmd(egui::ViewportCommand::WindowLevel(lvl));
    }

    /// The shortcut: brings Corvox up, or, if it already has focus, gives focus back to
    /// whatever window had it before.
    fn on_hotkey(&mut self) {
        let ctx = self.tx.1.clone();
        let focused = match self.win {
            Some(w) => w.is_foreground(),
            None => ctx.input(|i| i.viewport().focused.unwrap_or(false)),
        };
        if focused {
            let prev = self.prev_window.as_ref().and_then(|p| platform::Window::from_raw(p.load(std::sync::atomic::Ordering::Relaxed)));
            match (self.win, prev) {
                (Some(own), Some(prev)) if prev != own && prev.exists() => {
                    prev.focus();
                }
                (Some(_), _) => {}
                (None, _) => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
            }
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        match self.win {
            Some(w) => {
                w.focus();
            }
            None => ctx.send_viewport_cmd(egui::ViewportCommand::Focus),
        }
        self.focus_text = true;
    }

    fn import_voice(&mut self) {
        let Some(src) = rfd::FileDialog::new()
            .set_title(t("Pick a clip of your voice"))
            .add_filter(t("Audio"), &["mp3", "wav", "flac", "ogg", "m4a", "aac", "opus", "webm", "mp4"])
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
        // Download the freshly installed engine's model in the background. Switch to it only
        // if the current engine can't speak yet; otherwise just let the user pick it later.
        let current_usable = self.engine().map(|e| self.model_ready(e)).unwrap_or(false);
        let candidates: Vec<Engine> = cat.engines.iter().filter(|e| e.env == env).cloned().collect();
        let pick = candidates.iter().find(|e| e.languages.contains(&self.cfg.language)).or(candidates.first()).cloned();
        if let Some(e) = pick {
            if !current_usable {
                self.switch_to = Some(e.id.clone());
            }
            if self.model_ready(&e) {
                if !current_usable {
                    self.select_engine(&e);
                }
            } else {
                self.start_download(&e);
            }
        }
    }

    // ---------- events ----------

    fn handle_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::HelperReady { key } => {
                    if let Some(hs) = self.helpers.get_mut(&key) {
                        hs.ready = true;
                    }
                    if key == self.main_key {
                        self.status = Status::Ready;
                        self.preload(false);
                        if let Some(t) = self.queued.take() {
                            self.speak(t);
                        }
                    } else if let Some(dl) = self.downloads.get(&key).cloned() {
                        if let (Some(e), Some(hs)) = (self.engine_by_id(&dl.engine), self.helpers.get_mut(&key)) {
                            let _ = hs.h.load(&e.id, &dl.lang, None, e.model_mb);
                        }
                    }
                }
                Event::HelperExited { key, msg } => {
                    if self.spare_key.as_deref() == Some(key.as_str()) {
                        self.spare_key = None;
                        self.helpers.remove(&key);
                        continue;
                    }
                    if key == self.main_key {
                        if self.helpers.remove(&self.main_key).is_some() {
                            self.error = Some(Msg::new("Engine stopped. {}", &[&msg]));
                            self.status = Status::Ready;
                        }
                    } else if self.downloads.contains_key(&key) {
                        self.finish_download(&key, Err(Msg::new("Download stopped. {}", &[&msg])));
                    }
                }
                Event::HelperStatus { key, msg } => {
                    if key == self.main_key && self.current_req.is_none() {
                        self.status = Status::Msg(Msg::engine(msg));
                    }
                }
                Event::Status(s) => {
                    if let Some(rest) = s.strip_prefix("voice-ok:") {
                        self.importing = false;
                        let mut it = rest.rsplitn(2, ':');
                        let secs = it.next().unwrap_or("").to_string();
                        let name = it.next().unwrap_or("").to_string();
                        self.voices = config::list_voices();
                        self.cfg.voice = Some(name.clone());
                        self.cfg.save();
                        self.error = None;
                        self.status = Status::Msg(Msg::new("Added \"{}\" ({} s)", &[&name, &secs]));
                        self.preload(true);
                    } else if let Some(e) = s.strip_prefix("voice-err:") {
                        self.importing = false;
                        self.error = Some(Msg::new("Couldn't use that file: {}", &[e]));
                    }
                }
                Event::LoadDone { key, result } => {
                    if key == self.main_key {
                        let Some(hs) = self.helpers.get_mut(&self.main_key) else { continue };
                        let mk = hs.loading.take();
                        match result {
                            Ok(dev) => {
                                if let Some(mk) = mk {
                                    hs.loaded.insert(mk.clone(), dev);
                                    hs.order.retain(|k| *k != mk);
                                    hs.order.push(mk.clone());
                                    self.model_dl.remove(&mk);
                                    self.cfg.mark_downloaded(&mk);
                                    self.cfg.save();
                                }
                                if self.current_req.is_none() {
                                    self.status = Status::Ready;
                                }
                            }
                            Err(e) => {
                                if let Some(mk) = mk {
                                    self.model_dl.remove(&mk);
                                }
                                self.error = Some(Msg::raw(e));
                                self.status = Status::Ready;
                            }
                        }
                    } else if self.downloads.contains_key(&key) {
                        self.finish_download(&key, result.map_err(Msg::raw));
                    }
                }
                Event::ModelProgress { key, done_mb } => {
                    let mk = if key == self.main_key {
                        self.helpers.get(&self.main_key).and_then(|hs| hs.loading.clone())
                    } else {
                        self.downloads.get(&key).map(|d| d.key.clone())
                    };
                    if let Some(mk) = mk {
                        self.model_dl.insert(mk, done_mb);
                    }
                }
                Event::Chunk { req, samples, sr } => {
                    let mut flush = None;
                    if let Some(p) = &mut self.pending {
                        if p.req == req {
                            p.all.extend_from_slice(&samples);
                            p.sr = sr;
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
                            if p.req == req {
                                if !p.chunks.is_empty() {
                                    self.play_now(p.chunks);
                                }
                                if result.is_ok() {
                                    self.save_phrase_audio(&p.text, p.all, p.sr);
                                }
                            }
                        }
                        if let Err(e) = result {
                            self.error = Some(Msg::raw(e));
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
                    let inst = self.installs.remove(&env);
                    if self.resyncing.remove(&env) {
                        // Packages brought up to date after an app update. If that failed
                        // (offline?), the old ones usually still work: start anyway and try
                        // again next time.
                        if !ok && !cancelled {
                            let line = inst.map(|i| i.line).unwrap_or_default();
                            self.error = Some(Msg::new("Couldn't update the engine's packages: {}", &[&line]));
                        }
                        self.ensure_main();
                    } else if ok {
                        self.paused_installs.remove(&env);
                        self.status = Status::Ready;
                        self.after_install(&env);
                    } else if cancelled {
                        self.paused_installs.insert(env, inst.map(|i| i.mb).unwrap_or(0.0));
                    } else {
                        self.paused_installs.insert(env, inst.as_ref().map(|i| i.mb).unwrap_or(0.0));
                        self.error = Some(Msg::new("Install failed: {}", &[&inst.map(|i| i.line).unwrap_or_default()]));
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
                        Err(e) => self.error = Some(Msg::new("Test failed: {}", &[&e])),
                    }
                }
                Event::VmicLine(step) => {
                    self.vmic_step = Some(step);
                }
                Event::VmicDone { code } => {
                    self.vmic_step = None;
                    self.rebuild_player();
                    self.vmic_msg = Some(match code {
                        0 if self.player.has_mic() => {
                            self.cfg.vmic_setup = true;
                            self.cfg.save();
                            (true, Msg::new("Virtual mic ready. In Discord: Settings > Voice & Video > Input Device > {}.", &[vmic_discord_name()]))
                        }
                        0 => (true, Msg::new("Installed. Restart your PC to finish, then open Corvox again.", &[])),
                        2 => (false, Msg::new("Cancelled: the Windows permission prompt was declined.", &[])),
                        3 => (false, Msg::new("Stopped: the installer isn't signed by VB-Audio. Nothing was installed.", &[])),
                        4 => (false, Msg::new("Couldn't download VB-Cable. Check your internet connection.", &[])),
                        c => (false, Msg::new("Setup failed (code {}).", &[&c.to_string()])),
                    });
                }
                Event::Hotkey => self.on_hotkey(),
                Event::DeleteDone { engine, freed_mb, error } => {
                    self.deleting.remove(&engine);
                    let name = self.engine_by_id(&engine).map(|e| e.name).unwrap_or(engine);
                    match error {
                        None => self.status = Status::Msg(Msg::new("Deleted {} (freed {})", &[&name, &fmt_mb(freed_mb)])),
                        Some(e) => self.error = Some(Msg::new("Couldn't delete everything: {}", &[&e])),
                    }
                    self.fix_engine_for_language();
                    self.ensure_main();
                }
                Event::CacheCleaned { result } => {
                    self.cleaning_cache = false;
                    self.cache_msg = Some(match result {
                        Ok(line) => Msg::new("Download cache cleared. {}", &[&line]),
                        Err(e) => Msg::new("Couldn't clear the cache: {}", &[&e]),
                    });
                }
                Event::UpdateChecked { result, manual } => {
                    if matches!(self.upd, Upd::Downloading { .. } | Upd::Restarting) {
                        continue;
                    }
                    self.upd = match result {
                        Ok(Some(r)) => Upd::Available(r),
                        Ok(None) if manual => Upd::UpToDate,
                        Err(e) if manual => Upd::Failed(Msg::new("Couldn't check for updates: {}", &[&e])),
                        _ => Upd::Idle, // the quiet check at startup only speaks up when there's news
                    };
                }
                Event::UpdateProgress { done: d, total: tot } => {
                    if let Upd::Downloading { done, total, .. } = &mut self.upd {
                        *done = d;
                        *total = tot;
                    }
                }
                Event::UpdateDone { result } => match result {
                    Ok(()) => match update::restart() {
                        Ok(()) => {
                            self.upd = Upd::Restarting;
                            self.tx.1.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        Err(e) => self.upd = Upd::Failed(Msg::new("Updated, but couldn't restart ({}). Start Corvox again.", &[&e])),
                    },
                    Err(e) => {
                        self.upd = Upd::Failed(Msg::new("Update failed: {}", &[&e]));
                        self.ensure_main(); // the engine was stopped for the update
                    }
                },
            }
        }
    }

    fn update_ptt(&mut self) {
        let idle = !self.busy();
        if idle && !self.ptt.is_held() && matches!(self.status, Status::Speaking | Status::SpeakingSaved | Status::Generating) {
            self.status = Status::Ready;
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
                if matches!(self.status, Status::Speaking | Status::SpeakingSaved) {
                    self.status = Status::Ready;
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
            if self.cfg.snap.is_some() {
                self.set_snap(None); // moved by hand: no longer stuck to a corner
            }
        }
        ui.scope_builder(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)), |ui| {
            icons::small(ui, Icon::Mic, ui.visuals().text_color());
            ui.label(RichText::new(APP_NAME).color(ui.visuals().text_color()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icons::button(ui, Icon::Close, t("Close")).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                // Same spot as the bar's expand button, so switching back and forth needs no mouse move.
                if icons::button(ui, Icon::ChevronDown, t("Collapse to a bar")).clicked() {
                    self.expand(false);
                }
                let on_top = self.cfg.always_on_top;
                let (icon, tip) = if on_top {
                    (Icon::Pin, t("On top of other windows (click to turn off)"))
                } else {
                    (Icon::PinOff, t("Keep on top of other windows"))
                };
                let tint = if on_top { ui.visuals().selection.stroke.color } else { ui.visuals().weak_text_color() };
                if icons::button_tinted(ui, icon, tint, tip).clicked() {
                    self.set_on_top(!on_top);
                }
                let st_tint = if self.page == Page::Settings { ui.visuals().selection.stroke.color } else { ui.visuals().widgets.inactive.fg_stroke.color };
                if icons::button_tinted(ui, Icon::Sliders, st_tint, t("Settings")).clicked() {
                    self.page = if self.page == Page::Settings { Page::Main } else { Page::Settings };
                }
                let news = match &self.upd {
                    Upd::Available(r) => Some(r.version.clone()),
                    Upd::Downloading { version, .. } => Some(version.clone()),
                    _ => None,
                };
                if let Some(v) = news {
                    let accent = ui.visuals().selection.stroke.color;
                    if icons::button_tinted(ui, Icon::Download, accent, &tf("Corvox {} is available", &[&v])).clicked() {
                        self.page = Page::Settings;
                    }
                }
            });
        });
    }

    fn text_box(&mut self, ui: &mut egui::Ui, width: f32) {
        let up = ui.input(|i| i.key_pressed(Key::ArrowUp));
        let down = ui.input(|i| i.key_pressed(Key::ArrowDown));
        let resp = ui.add(
            egui::TextEdit::singleline(&mut self.text)
                .hint_text(t("Type and press Enter…"))
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

    /// Download progress of the selected engine's model, as text, if one is running.
    fn selected_download(&self) -> Option<String> {
        let e = self.engine()?;
        let key = self.switch_to.as_deref().and_then(|id| self.engine_by_id(id)).map(|e| self.model_key_for(&e)).unwrap_or_else(|| self.model_key_for(e));
        let total = self.switch_to.as_deref().and_then(|id| self.engine_by_id(id)).map(|e| e.model_mb).unwrap_or(e.model_mb);
        self.model_dl.get(&key).map(|done| pct_or_mb(*done, total))
    }

    fn status_text(&self) -> String {
        if let Some(p) = self.selected_download() {
            return tf("Downloading model {}", &[&p]);
        }
        // Any other background download or install, so it never looks finished early.
        if let Some(dl) = self.downloads.values().next() {
            if let Some(e) = self.engine_by_id(&dl.engine) {
                let p = self.model_dl.get(&dl.key).map(|d| pct_or_mb(*d, e.model_mb)).unwrap_or_default();
                return tf("Downloading {} {}", &[&short_name(&e.name), &p]);
            }
        }
        if let Some(env) = self.installs.keys().next() {
            let name = self.catalog.as_ref().and_then(|c| c.engines.iter().find(|e| &e.env == env)).map(|e| short_name(&e.name)).unwrap_or_default();
            if self.resyncing.contains(env) {
                return tf("Updating {} packages…", &[&name]);
            }
            return tf("Installing {}…", &[&name]);
        }
        self.status.text()
    }

    fn status_chip(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        if busy && icons::labeled(ui, Icon::Stop, t("Stop")).on_hover_text(t("Stop speaking (Esc)")).clicked() {
            self.stop();
        }
        let text = self.status_text();
        let col = if self.error.is_some() {
            RED
        } else if busy || self.status != Status::Ready || !self.downloads.is_empty() || !self.installs.is_empty() {
            AMBER
        } else {
            GREEN
        };
        ui.label(RichText::new(truncate(&text, 30)).color(col).small());
        let (r, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
        ui.painter().circle_filled(r.center(), 3.5, col);
    }

    fn error_line(&mut self, ui: &mut egui::Ui) {
        if let Some(e) = self.error.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(e.text()).color(RED).small());
                if icons::button(ui, Icon::Close, t("Dismiss")).clicked() {
                    self.error = None;
                }
            });
        }
    }

    /// Speaker icon (mute/unmute your own speakers) and their volume.
    fn volume_control(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().slider_width = 70.0;
        let mut changed_gain = false;
        if self.cfg.monitor {
            ui.add_sized([32.0, 16.0], egui::Label::new(RichText::new(format!("{:.0}%", self.cfg.monitor_volume * 100.0)).small()));
            let r = ui
                .add(egui::Slider::new(&mut self.cfg.monitor_volume, 0.0..=1.0).show_value(false))
                .on_hover_text(t("Volume on your speakers/headphones. Discord always gets full volume."));
            changed_gain = r.changed();
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                self.cfg.save();
            }
        }
        let (icon, tip) = if self.cfg.monitor { (Icon::Volume, t("Hearing yourself: on (click to mute)")) } else { (Icon::VolumeOff, t("Hearing yourself: off (click to unmute)")) };
        if icons::button(ui, icon, tip).clicked() {
            self.cfg.monitor = !self.cfg.monitor;
            self.cfg.save();
            self.rebuild_player();
        }
        if changed_gain {
            self.player.set_monitor_gain(self.cfg.monitor_volume);
        }
    }

    fn main_page(&mut self, ui: &mut egui::Ui) {
        let w = ui.available_width();
        self.text_box(ui, w);
        ui.add_space(2.0);
        self.error_line(ui);
        self.vmic_notice(ui);
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let label = if self.cfg.keep_phrases { t("Recent") } else { t("Pinned") };
            ui.label(RichText::new(label).small().weak()).on_hover_text(t("↑↓ to choose · Enter speaks · right-click to edit"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| self.volume_control(ui));
        });

        let list = self.phrase_list();
        let keys: Vec<Option<String>> = list.iter().map(|p| self.phrase_key(p)).collect();
        let bottom_h = 38.0;
        let mut speak = None;
        let mut pin = None;
        let mut remove = None;
        let mut edit = None;
        let history_pos = self.history_pos;
        let pinned_list = self.cfg.pinned.clone();
        let cached = self.cached.clone();
        egui::ScrollArea::vertical().max_height(ui.available_height() - bottom_h).auto_shrink([false, false]).show(ui, |ui| {
            if list.is_empty() {
                ui.add_space(12.0);
                let hint = if self.cfg.keep_phrases {
                    t("Phrases you speak show up here. Click one to say it again.")
                } else {
                    t("Phrase history is off. Pin a phrase to keep it here.")
                };
                ui.label(RichText::new(hint).weak());
            }
            for (i, p) in list.iter().enumerate() {
                let pinned = pinned_list.contains(p);
                let selected = history_pos == Some(i);
                let saved = keys[i].as_ref().map(|k| cached.contains(k)).unwrap_or(false);
                ui.horizontal(|ui| {
                    let (tint, tip) = if saved {
                        (ui.visuals().selection.stroke.color, t("Speak (saved audio, plays instantly)"))
                    } else {
                        (ui.visuals().widgets.inactive.fg_stroke.color, t("Speak"))
                    };
                    if icons::button_tinted(ui, Icon::Play, tint, tip).clicked() {
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
                        .on_hover_text(t("Click to speak, right-click to edit"));
                    if r.clicked() {
                        speak = Some(p.clone());
                    }
                    if r.secondary_clicked() {
                        edit = Some(p.clone());
                    }
                    let tint = if pinned { ui.visuals().selection.stroke.color } else { ui.visuals().weak_text_color() };
                    if icons::button_tinted(ui, Icon::Pin, tint, if pinned { t("Unpin") } else { t("Pin") }).clicked() {
                        pin = Some(p.clone());
                    }
                    if !pinned && icons::button_tinted(ui, Icon::Close, ui.visuals().weak_text_color(), t("Remove")).clicked() {
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
            let removed = self.cfg.trim_recent();
            self.forget_phrases(&removed);
            self.cfg.save();
        }
        if let Some(p) = remove {
            self.cfg.recent.retain(|r| *r != p);
            self.forget_phrases(&[p]);
            self.cfg.save();
        }
        if let Some(p) = speak {
            self.speak(p);
        }

        ui.separator();
        ui.horizontal(|ui| {
            let voice = self.cfg.voice.clone().unwrap_or_else(|| t("Add voice").into());
            if icons::labeled(ui, Icon::User, &truncate(&voice, 14)).on_hover_text(t("Voices")).clicked() {
                self.page = Page::Voices;
            }
            self.language_combo(ui);
            let ename = self.engine().map(|e| short_name(&e.name)).unwrap_or_else(|| t("Engine").into());
            if icons::labeled(ui, Icon::Cpu, &ename).on_hover_text(t("Engines and their stats")).clicked() {
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
                    let name = i18n::lang_name(&code, cat.lang_name(&code));
                    ui.selectable_value(&mut self.cfg.language, code.clone(), name);
                }
            })
            .response
            .on_hover_text(t("Speech language"));
        if self.cfg.language != before {
            self.fix_engine_for_language();
            self.cfg.save();
            self.ensure_main();
        }
    }

    fn start_vmic_setup(&mut self, ctx: &egui::Context) {
        if cfg!(target_os = "macos") {
            ctx.open_url(egui::OpenUrl::new_tab(vmic::BLACKHOLE_URL));
            self.vmic_msg = Some((true, Msg::new("Install BlackHole 2ch from that page, then click Refresh here.", &[])));
            return;
        }
        self.vmic_msg = None;
        self.vmic_step = Some("Starting…".into());
        vmic::setup(self.tx.clone());
    }

    /// One line on the main page while there's no virtual mic (or setup is running/finished).
    fn vmic_notice(&mut self, ui: &mut egui::Ui) {
        if let Some(step) = self.vmic_step.clone() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(i18n::vmic_step(&step)).small());
            });
        } else if let Some((ok, msg)) = self.vmic_msg.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(msg.text()).small().color(if ok { GREEN } else { RED }));
                if icons::button(ui, Icon::Close, t("Dismiss")).clicked() {
                    self.vmic_msg = None;
                }
            });
        } else if !self.player.has_mic() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("No virtual mic yet, so Discord can't hear Corvox.")).small().color(GOLD));
                if ui.small_button(t("Set up")).clicked() {
                    self.start_vmic_setup(&ui.ctx().clone());
                }
            });
        }
    }

    /// The fuller explanation and button in Settings.
    fn vmic_box(&mut self, ui: &mut egui::Ui) {
        if let Some(step) = self.vmic_step.clone() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(i18n::vmic_step(&step)).small());
            });
            return;
        }
        if let Some((ok, msg)) = self.vmic_msg.clone() {
            ui.label(RichText::new(msg.text()).small().color(if ok { GREEN } else { RED }));
        }
        if self.player.has_mic() {
            return;
        }
        ui.label(
            RichText::new(if cfg!(windows) {
                t("Discord needs a microphone to hear Corvox. VB-Cable is a free virtual audio cable from VB-Audio: Corvox plays into it and Discord listens to it. Windows will ask for permission, then click Install Driver in VB-Audio's installer.")
            } else if cfg!(target_os = "macos") {
                t("Discord needs a microphone to hear Corvox. BlackHole is a free virtual audio cable: install BlackHole 2ch, and Corvox plays into it while Discord listens to it.")
            } else {
                t("Discord needs a microphone to hear Corvox. Corvox can create a virtual mic with PipeWire/PulseAudio, no install needed.")
            })
            .small()
            .color(GOLD),
        );
        let label = if cfg!(target_os = "macos") { t("Get BlackHole") } else { t("Set up virtual mic") };
        if icons::labeled(ui, Icon::Download, label).clicked() {
            self.start_vmic_setup(&ui.ctx().clone());
        }
    }

    fn back_header(&mut self, ui: &mut egui::Ui, title: &str) {
        ui.horizontal(|ui| {
            if icons::labeled(ui, Icon::Back, t("Back")).clicked() {
                self.page = Page::Main;
            }
            ui.label(RichText::new(title).strong());
        });
    }

    fn engines_page(&mut self, ui: &mut egui::Ui) {
        let Some(cat) = self.catalog.clone() else {
            self.back_header(ui, t("Engines"));
            ui.label(t("Couldn't find engines/catalog.json next to the app."));
            return;
        };
        ui.horizontal(|ui| {
            if icons::labeled(ui, Icon::Back, t("Back")).clicked() {
                self.page = Page::Main;
            }
            let lname = i18n::lang_name(&self.cfg.language, cat.lang_name(&self.cfg.language));
            ui.label(RichText::new(tf("Engines · {}", &[&lname])).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.checkbox(&mut self.show_all_engines, t("All languages"));
            });
        });
        if self.uv.is_none() {
            ui.label(RichText::new(t("uv isn't installed, so engines can't be installed. See the README.")).color(RED));
        }
        self.error_line(ui);
        let list: Vec<Engine> = if self.show_all_engines { cat.engines.clone() } else { cat.engines_for(&self.cfg.language).into_iter().cloned().collect() };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for e in &list {
                self.engine_card(ui, &cat, e);
                ui.add_space(4.0);
            }
            ui.label(
                RichText::new(t("Voice match compares the output to your clip. Clarity is how well Whisper understands it. Speed is seconds of speech per second of work."))
                    .small()
                    .weak(),
            );
        });
    }

    fn engine_stats(&self, ui: &mut egui::Ui, cat: &Catalog, e: &Engine) {
        let measured = self.cfg.bench.get(&e.id).cloned();
        let b = measured.clone().unwrap_or_default();
        let p = &e.published;
        let pick = |m: Option<f32>, pb: Option<f32>| if measured.is_some() { m } else { pb };
        let vm = pick(b.voice_match, p.voice_match);
        let cl = pick(b.clarity, p.clarity);
        let sp = pick(b.speed, p.speed);
        let ram = pick(b.ram_mb, p.ram_mb);
        let vram = pick(b.vram_mb, p.vram_mb);
        stat_row(ui, t("Voice match"), vm.map(|v| v / 100.0), vm.map(|v| format!("{v:.0}%")));
        stat_row(ui, t("Clarity"), cl.map(|v| v / 100.0), cl.map(|v| format!("{v:.0}%")));
        stat_row(
            ui,
            t("Speed"),
            sp.map(|v| (v / 4.0).min(1.0)),
            sp.map(|v| if v < 2.0 { tf("{}× real time", &[&format!("{v:.2}")]) } else { tf("{}× real time", &[&format!("{v:.1}")]) }),
        );
        if e.is_gpu() {
            let label = if measured.is_some() && vram.is_none() { t("VRAM (ran on CPU)") } else { "VRAM" };
            stat_row(ui, label, vram.map(|v| (v / 8192.0).min(1.0)), vram.map(fmt_mb));
            stat_row(ui, "RAM", ram.map(|v| (v / 16384.0).min(1.0)), ram.map(fmt_mb));
        } else {
            stat_row(ui, "RAM", ram.map(|v| (v / 8192.0).min(1.0)), ram.map(fmt_mb));
        }
        let src = match &measured {
            Some(b) => tf(
                "Measured on your PC · {} · voice \"{}\"{}",
                &[&i18n::lang_name(&b.language, cat.lang_name(&b.language)), &b.voice, if b.int8 { " · int8" } else { "" }],
            ),
            None => tf("Published · {}", &[if e.published.source.is_empty() { t("no numbers yet") } else { &e.published.source }]),
        };
        ui.label(RichText::new(src).small().weak());
    }

    fn engine_card(&mut self, ui: &mut egui::Ui, cat: &Catalog, e: &Engine) {
        let installed = engine::is_installed(&e.env);
        let mkey = self.model_key_for(e);
        let downloaded = self.cfg.is_downloaded(&mkey);
        let selected = self.cfg.engine == e.id;
        let waiting = self.switch_to.as_deref() == Some(e.id.as_str());
        let speaks_lang = e.languages.contains(&self.cfg.language);
        let ui_lang = self.cfg.ui_lang.clone();
        let stroke = if selected || waiting {
            Stroke::new(1.5, ui.visuals().selection.stroke.color)
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke
        };
        let card = egui::Frame::new().stroke(stroke).corner_radius(CornerRadius::same(8)).inner_margin(8.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let can_select = installed && speaks_lang;
                let r = ui.add_enabled(can_select, egui::RadioButton::new(selected, RichText::new(&e.name).strong()));
                if r.clicked() && !selected {
                    self.select_engine(e);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let (txt, col) = if e.is_gpu() { ("GPU", GOLD) } else { (t("CPU"), GREEN) };
                    ui.label(RichText::new(txt).small().color(col).strong());
                });
            });
            ui.label(RichText::new(format!("{} · {}", e.by, e.note_for(&ui_lang))).small().weak());
            if e.requires_hf && !config::has_hf_token() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(t("Needs a free Hugging Face login to clone voices.")).small().color(GOLD));
                    if ui.small_button(t("Set up")).clicked() {
                        self.page = Page::Settings;
                    }
                });
            }

            // Stats; dimmed with a short description on top until the engine is set up.
            let ready = installed && downloaded;
            let stats = ui.scope(|ui| {
                if !ready {
                    ui.multiply_opacity(0.25);
                }
                self.engine_stats(ui, cat, e);
            });
            if !ready {
                let rect = stats.response.rect;
                let bg = ui.visuals().panel_fill.gamma_multiply(0.6);
                ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
                ui.put(
                    rect.shrink2(Vec2::new(10.0, 4.0)),
                    egui::Label::new(RichText::new(e.summary_for(&ui_lang)).strong()).wrap().halign(Align::Center),
                );
            }
            let on_cpu = self.helpers.get(&self.main_key).and_then(|hs| hs.loaded.get(&mkey)).map(|d| d == "cpu").unwrap_or(false);
            if on_cpu && e.is_gpu() && selected {
                ui.label(RichText::new(t("No CUDA GPU found, so this engine is running on the CPU.")).small().color(GOLD));
            }

            self.engine_actions(ui, cat, e, installed, downloaded, speaks_lang, &mkey, waiting);
        });
        // Bring an engine into view when its install starts.
        if self.installs.contains_key(&e.env) && self.scroll_to.as_deref() == Some(e.env.as_str()) {
            card.response.scroll_to_me(Some(Align::Center));
            self.scroll_to = None;
        }
    }

    /// Delete button (with a confirmation step) for an engine that has files on disk.
    fn delete_row(&mut self, ui: &mut egui::Ui, cat: &Catalog, e: &Engine) {
        if self.deleting.contains(&e.id) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(t("Deleting…")).small());
            });
            return;
        }
        if self.delete_prompt.as_deref() != Some(e.id.as_str()) {
            return;
        }
        let shared: Vec<String> = cat.engines.iter().filter(|o| o.env == e.env && o.id != e.id).map(|o| o.name.clone()).collect();
        let mb = if config::envs_dir().join(&e.env).exists() { e.env_mb } else { 0 } + e.model_mb;
        ui.add_space(2.0);
        ui.label(RichText::new(tf("Delete {}'s packages and model (about {})? You can install it again later.", &[&e.name, &fmt_mb(mb as f32)])).small().color(GOLD));
        if !shared.is_empty() {
            ui.label(RichText::new(tf("This also removes {}, which share the same install.", &[&shared.join(", ")])).small().color(GOLD));
        }
        ui.horizontal(|ui| {
            if icons::labeled(ui, Icon::Trash, t("Delete")).clicked() {
                self.delete_prompt = None;
                self.delete_engine(e);
            }
            if ui.button(t("Cancel")).clicked() {
                self.delete_prompt = None;
            }
        });
    }

    fn delete_button(&mut self, ui: &mut egui::Ui, e: &Engine) {
        if self.has_files(e) && !self.deleting.contains(&e.id) && icons::button(ui, Icon::Trash, t("Delete this engine's files")).clicked() {
            self.delete_prompt = Some(e.id.clone());
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn engine_actions(&mut self, ui: &mut egui::Ui, cat: &Catalog, e: &Engine, installed: bool, downloaded: bool, speaks_lang: bool, mkey: &str, waiting: bool) {
        self.delete_row(ui, cat, e);
        if self.deleting.contains(&e.id) {
            return;
        }
        // Step 1: Python packages.
        if let Some(inst) = self.installs.get(&e.env) {
            let frac = (inst.mb / e.env_mb as f32).min(0.99);
            ui.add_space(2.0);
            ui.label(RichText::new(t("Step 1 of 2 · Installing Python packages")).small());
            ui.add(egui::ProgressBar::new(frac).desired_height(12.0).text(RichText::new(tf("{} of ~{}", &[&fmt_mb(inst.mb), &fmt_mb(e.env_mb as f32)])).small()));
            let mut pause = false;
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{} · {}", fmt_elapsed(inst.started.elapsed()), truncate(&inst.line, 40))).small().weak());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    pause = icons::labeled(ui, Icon::Pause, t("Pause")).clicked();
                });
            });
            if pause {
                engine::cancel_install(&inst.handle);
            }
            return;
        }
        if let Some(mb) = self.paused_installs.get(&e.env).copied() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tf("Install paused · {} of ~{}", &[&fmt_mb(mb), &fmt_mb(e.env_mb as f32)])).small().color(GOLD));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    self.delete_button(ui, e);
                    if icons::labeled(ui, Icon::Play, t("Resume")).clicked() {
                        self.start_install(&e.env);
                    }
                });
            });
            return;
        }
        // Step 2: the model.
        if installed && !downloaded {
            if let Some(done) = self.model_dl.get(mkey).copied() {
                let frac = (done / e.model_mb as f32).min(0.99);
                ui.add_space(2.0);
                let label = if waiting { t("Step 2 of 2 · Downloading the model (switches when done)") } else { t("Step 2 of 2 · Downloading the model") };
                ui.label(RichText::new(label).small());
                ui.add(egui::ProgressBar::new(frac).desired_height(12.0).text(RichText::new(tf("{} of ~{}", &[&fmt_mb(done), &fmt_mb(e.model_mb as f32)])).small()));
                let background = self.download_helper(mkey).is_some();
                if background {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t("Your current engine keeps working meanwhile.")).small().weak());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icons::labeled(ui, Icon::Pause, t("Pause")).clicked() {
                                self.pause_download(mkey);
                            }
                        });
                    });
                }
                return;
            }
            let paused = self.paused_downloads.get(mkey).copied();
            ui.horizontal(|ui| {
                let text = match paused {
                    Some(mb) if mb > 0.0 => tf("Model download paused · {} of ~{}", &[&fmt_mb(mb), &fmt_mb(e.model_mb as f32)]),
                    Some(_) => t("Model download paused").into(),
                    None => tf("Installed · model not downloaded yet (~{})", &[&fmt_mb(e.model_mb as f32)]),
                };
                ui.label(RichText::new(text).small().color(GOLD));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    self.delete_button(ui, e);
                    let (icon, label) = if paused.is_some() { (Icon::Play, t("Resume")) } else { (Icon::Download, t("Download")) };
                    if icons::labeled(ui, icon, label).clicked() {
                        self.start_download(e);
                    }
                });
            });
            return;
        }
        ui.horizontal_wrapped(|ui| {
            if !installed {
                if self.agree_prompt.as_deref() == Some(e.id.as_str()) {
                    ui.label(RichText::new(&e.license).small());
                    if ui.button(t("I agree, install")).clicked() {
                        self.cfg.agreed.push(e.id.clone());
                        self.cfg.save();
                        self.agree_prompt = None;
                        self.start_install(&e.env);
                    }
                    if ui.button(t("Cancel")).clicked() {
                        self.agree_prompt = None;
                    }
                } else {
                    let r = ui.add_enabled_ui(self.uv.is_some(), |ui| icons::labeled(ui, Icon::Download, t("Install"))).inner;
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
                ui.label(RichText::new(i18n::engine_status(msg)).small());
            } else if speaks_lang {
                let can = self.cfg.voice_path().is_some();
                let r = ui.add_enabled_ui(can, |ui| icons::labeled(ui, Icon::Gauge, t("Test on my PC"))).inner;
                if r.on_disabled_hover_text(t("Add a voice clip first")).clicked() {
                    self.start_bench(e);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                self.delete_button(ui, e);
                ui.hyperlink_to(RichText::new(t("info")).small(), &e.url);
                let size = if installed {
                    t("installed").to_string()
                } else {
                    let shared = cat.engines.iter().any(|o| o.env == e.env && o.id != e.id && engine::is_installed(&o.env));
                    if shared {
                        tf("~{} model", &[&fmt_mb(e.model_mb as f32)])
                    } else {
                        tf("~{} on disk", &[&fmt_mb((e.env_mb + e.model_mb) as f32)])
                    }
                };
                ui.label(RichText::new(size).small().weak());
            });
        });
    }

    /// After an app update, engines whose package list changed get their packages brought up
    /// to date before they start (quick when little changed).
    fn resync_outdated(&mut self) {
        let (Some(cat), Some(dir)) = (self.catalog.clone(), self.engines_dir.clone()) else { return };
        let envs: HashSet<String> = cat.engines.iter().map(|e| e.env.clone()).collect();
        for env in envs {
            if self.uv.is_some() && engine::is_installed(&env) && engine::is_outdated(&dir, &env) && !self.installs.contains_key(&env) {
                self.resyncing.insert(env.clone());
                self.start_install(&env);
            }
        }
    }

    fn updates_section(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new(t("Updates")).small().weak());
        let busy = matches!(self.upd, Upd::Checking | Upd::Downloading { .. } | Upd::Restarting);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tf("Version {}", &[update::VERSION])).small());
            match &self.upd {
                Upd::Checking => {
                    ui.spinner();
                }
                Upd::UpToDate => {
                    ui.label(RichText::new(t("Up to date")).small().color(GREEN));
                }
                _ => {}
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add_enabled(!busy, egui::Button::new(RichText::new(t("Check now")).small())).clicked() {
                    self.upd = Upd::Checking;
                    update::check(self.tx.clone(), true);
                }
            });
        });
        match self.upd.clone() {
            Upd::Available(r) => {
                let accent = ui.visuals().selection.stroke.color;
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(tf("Corvox {} is available", &[&r.version])).small().color(accent));
                    if r.asset.is_some() && self.self_update_ok {
                        let blocked = !self.benching.is_empty() || !self.deleting.is_empty() || self.cleaning_cache;
                        let resp = ui.add_enabled_ui(!blocked, |ui| icons::labeled(ui, Icon::Download, t("Update and restart"))).inner;
                        if resp.on_disabled_hover_text(t("Wait for the running task to finish")).clicked() {
                            self.start_update(r.clone());
                        }
                    }
                    ui.hyperlink_to(RichText::new(t("Release page")).small(), &r.page);
                });
                let note = if r.asset.is_none() {
                    Some(t("That release has no download for this system yet."))
                } else if !self.self_update_ok {
                    Some(t("This copy can't update itself (it isn't a release folder, or Corvox can't write there). Get the new version from the release page."))
                } else {
                    None
                };
                if let Some(n) = note {
                    ui.label(RichText::new(n).small().weak());
                }
            }
            Upd::Downloading { version, done, total } => {
                let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                let text = tf("Downloading Corvox {}: {} of {}", &[&version, &fmt_mb(done as f32 / 1_048_576.0), &fmt_mb(total as f32 / 1_048_576.0)]);
                ui.add(egui::ProgressBar::new(frac).desired_height(12.0).text(RichText::new(text).small()));
            }
            Upd::Restarting => {
                ui.label(RichText::new(t("Restarting…")).small());
            }
            Upd::Failed(m) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(m.text()).small().color(RED));
                    ui.hyperlink_to(RichText::new(t("Release page")).small(), update::releases_page());
                });
            }
            _ => {}
        }
        if ui.checkbox(&mut self.cfg.check_updates, RichText::new(t("Check for updates when Corvox starts")).small()).changed() {
            self.cfg.save();
        }
    }

    /// Stops everything running from the app folder (so its files can be replaced), then
    /// downloads and installs the release in the background.
    fn start_update(&mut self, r: update::Release) {
        let Some(asset) = r.asset.clone() else { return };
        self.stop();
        self.helpers.clear();
        self.main_key.clear();
        self.spare_key = None;
        self.downloads.clear();
        for (_, inst) in self.installs.drain() {
            engine::cancel_install(&inst.handle);
        }
        self.resyncing.clear();
        self.cfg.save();
        self.upd = Upd::Downloading { version: r.version.clone(), done: 0, total: asset.size };
        update::apply(asset, self.tx.clone());
    }

    fn start_install(&mut self, env: &str) {
        let (Some(uv), Some(dir)) = (self.uv.clone(), self.engines_dir.clone()) else { return };
        let handle = engine::install(uv, dir, env.to_string(), self.tx.clone());
        self.scroll_to = Some(env.to_string());
        let prev = self.paused_installs.remove(env).unwrap_or(0.0);
        self.installs.insert(env.to_string(), Install { started: Instant::now(), mb: prev, line: t("Starting…").into(), handle });
    }

    fn start_bench(&mut self, e: &Engine) {
        let (Some(uv), Some(dir), Some(voice)) = (self.uv.clone(), self.engines_dir.clone(), self.cfg.voice_path()) else { return };
        self.benching.insert(e.id.clone(), "Starting…".into());
        engine::bench(uv, dir, e.env.clone(), e.id.clone(), voice, self.cfg.language.clone(), self.agreed(&e.id), self.cfg.int8, self.tx.clone());
    }

    fn voices_page(&mut self, ui: &mut egui::Ui) {
        self.back_header(ui, t("Voices"));
        ui.label(
            RichText::new(t("Use 10–20 seconds of you talking normally: no music, no background noise. Corvox uses the part that ends at a natural pause."))
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
                ui.label(RichText::new(tf("{} s clip", &[&format!("{secs:.0}")])).small().weak());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icons::button(ui, Icon::Trash, t("Delete")).clicked() {
                        delete = Some(v.clone());
                    }
                    if icons::button(ui, Icon::Play, t("Play the clip")).clicked() {
                        play = Some(v.clone());
                    }
                });
            });
        }
        if let Some(v) = select {
            self.cfg.voice = Some(v);
            self.cfg.save();
            self.tidy_phrase_cache();
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
                ui.label(t("Preparing the clip…"));
            } else if icons::labeled(ui, Icon::Plus, t("Add voice from a file…")).clicked() {
                self.import_voice();
            }
        });
    }

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        self.back_header(ui, t("Settings"));
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(4.0);
            self.updates_section(ui);

            ui.add_space(8.0);
            ui.label(RichText::new(t("Interface language")).small().weak());
            let before = self.cfg.ui_lang.clone();
            egui::ComboBox::from_id_salt("ui_lang").selected_text(if self.cfg.ui_lang == "tr" { "Türkçe" } else { "English" }).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.cfg.ui_lang, "en".to_string(), "English");
                ui.selectable_value(&mut self.cfg.ui_lang, "tr".to_string(), "Türkçe");
            });
            if self.cfg.ui_lang != before {
                i18n::set_lang(&self.cfg.ui_lang);
                self.cfg.save();
            }

            ui.add_space(8.0);
            ui.label(RichText::new(t("Send the voice to")).small().weak());
            let auto = audio::guess_virtual_device(&self.devices);
            let current = self.cfg.output_device.clone();
            let shown = current.clone().unwrap_or_else(|| tf("Auto ({})", &[&auto.clone().unwrap_or_else(|| t("no virtual cable found").into())]));
            let mut changed = false;
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("dev").selected_text(truncate(&shown, 40)).width(300.0).show_ui(ui, |ui| {
                    if ui.selectable_label(current.is_none(), t("Auto")).clicked() {
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
                if icons::button(ui, Icon::Refresh, t("Refresh devices")).clicked() {
                    changed = true;
                }
            });
            self.vmic_box(ui);
            if let Some(e) = &self.player.error {
                ui.label(RichText::new(e).small().color(RED));
            }
            if changed {
                self.cfg.save();
                self.rebuild_player();
            }

            ui.add_space(8.0);
            ui.label(RichText::new(t("Phrase history")).small().weak());
            if ui.checkbox(&mut self.cfg.keep_phrases, t("Remember recent phrases and their audio")).changed() {
                if !self.cfg.keep_phrases {
                    self.cfg.recent.clear();
                }
                self.cfg.save();
                self.tidy_phrase_cache();
            }
            if self.cfg.keep_phrases {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("Keep the last")).small());
                    let r = ui.add(egui::DragValue::new(&mut self.cfg.phrase_limit).range(1..=config::MAX_PHRASE_LIMIT));
                    ui.label(RichText::new(t("phrases")).small());
                    if r.changed() {
                        let removed = self.cfg.trim_recent();
                        self.forget_phrases(&removed);
                        self.cfg.save();
                    }
                });
                ui.label(
                    RichText::new(t("Saved audio replays instantly without the engine. Older phrases and their audio are deleted; pinned ones stay. Turning this off clears the list and the saved audio."))
                        .small()
                        .weak(),
                );
            }

            ui.add_space(8.0);
            if ui
                .checkbox(&mut self.cfg.int8, t("Faster, lighter Pocket engines (int8)"))
                .on_hover_text(t("About 10% faster and ~450 MB less RAM. Re-run Test on my PC to compare."))
                .changed()
            {
                self.cfg.save();
                self.restart_main();
            }

            ui.add_space(8.0);
            ui.label(RichText::new(t("Push-to-talk key (held while speaking)")).small().weak());
            let mut ptt_changed = false;
            egui::ComboBox::from_id_salt("ptt").selected_text(self.cfg.ptt_key.clone().unwrap_or_else(|| t("Off").into())).show_ui(ui, |ui| {
                ptt_changed |= ui.selectable_value(&mut self.cfg.ptt_key, None, t("Off")).changed();
                for k in ptt::KEYS.into_iter().filter(|k| ptt::supported(k)) {
                    ptt_changed |= ui.selectable_value(&mut self.cfg.ptt_key, Some(k.to_string()), k).changed();
                }
            });
            ui.label(RichText::new(t("Set the same key as Push to Talk in Discord. F13–F24 don't clash with anything.")).small().weak());
            if ptt_changed {
                self.cfg.save();
            }

            ui.add_space(8.0);
            ui.label(RichText::new(t("Shortcut: bring Corvox up, press again to go back")).small().weak());
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.hotkey_text).desired_width(180.0));
                if ui.button(t("Apply")).clicked() {
                    self.cfg.hotkey = self.hotkey_text.trim().to_string();
                    self.cfg.save();
                    self.error = None;
                    self.apply_hotkey();
                }
            });
            self.error_line(ui);

            ui.add_space(8.0);
            ui.label(RichText::new(t("Hugging Face login (for Kyutai's Pocket TTS voices)")).small().weak());
            if config::has_hf_token() {
                ui.horizontal(|ui| {
                    icons::small(ui, Icon::Check, GREEN);
                    ui.label(RichText::new(t("Token saved")).small().color(GREEN));
                });
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("1.").small());
                ui.hyperlink_to(RichText::new(t("Accept the terms on kyutai/pocket-tts")).small(), "https://huggingface.co/kyutai/pocket-tts");
                ui.label(RichText::new("2.").small());
                ui.hyperlink_to(RichText::new(t("Create a read token")).small(), "https://huggingface.co/settings/tokens");
            });
            let mut save = false;
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.hf_input).password(true).hint_text("hf_…").desired_width(220.0));
                save = ui.button(t("Save token")).clicked() && !self.hf_input.trim().is_empty();
            });
            if save {
                self.hf_msg = Some(match config::save_hf_token(&self.hf_input) {
                    Ok(()) => {
                        self.hf_input.clear();
                        self.restart_main(); // reload models with the cloning weights
                        Msg::new("Saved. Restarting the engine.", &[])
                    }
                    Err(e) => Msg::new("Couldn't save: {}", &[&e.to_string()]),
                });
            }
            if let Some(m) = &self.hf_msg {
                ui.label(RichText::new(m.text()).small().weak());
            }

            ui.add_space(8.0);
            ui.label(RichText::new(t("Download cache")).small().weak());
            ui.label(
                RichText::new(t("uv keeps downloaded packages so reinstalling is fast. Clearing it frees space; installed engines keep working."))
                    .small()
                    .weak(),
            );
            ui.horizontal(|ui| {
                if self.cleaning_cache {
                    ui.spinner();
                    ui.label(RichText::new(t("Clearing…")).small());
                } else {
                    let can = self.uv.is_some() && self.installs.is_empty();
                    let r = ui.add_enabled_ui(can, |ui| icons::labeled(ui, Icon::Trash, t("Clear download cache"))).inner;
                    if r.on_disabled_hover_text(t("Wait for installs to finish")).clicked() {
                        if let Some(uv) = self.uv.clone() {
                            self.cleaning_cache = true;
                            self.cache_msg = None;
                            engine::clean_uv_cache(uv, self.tx.clone());
                        }
                    }
                }
            });
            if let Some(m) = &self.cache_msg {
                ui.label(RichText::new(m.text()).small().weak());
            }

            ui.add_space(8.0);
            ui.label(RichText::new(t("Paths")).small().weak());
            let eng = self.engines_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| t("not found").into());
            let uv = self.uv.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| t("not installed").into());
            ui.label(RichText::new(tf("Engines: {}", &[&eng])).small());
            ui.label(RichText::new(format!("uv: {uv}")).small());
            ui.label(RichText::new(tf("Data: {}", &[&config::data_dir().display().to_string()])).small());
        });
    }

    fn pill(&mut self, ui: &mut egui::Ui) {
        // The whole bar drags the window. Registered first, so the text box and buttons drawn
        // after it still get their clicks.
        let all = ui.ctx().viewport_rect();
        let drag = ui.interact(all, ui.scope_id().with("bar-drag"), Sense::click_and_drag());
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            if self.cfg.snap.is_some() {
                self.set_snap(None);
            }
        }
        ui.horizontal_centered(|ui| {
            let tint = if self.busy() { AMBER } else { ui.visuals().weak_text_color() };
            icons::small(ui, Icon::Mic, tint);
            let tag = match self.selected_download() {
                Some(p) => tf("Downloading {}", &[&p]),
                None => format!("{} · {}", self.cfg.language.to_uppercase(), self.engine().map(|e| short_name(&e.name)).unwrap_or_default()),
            };
            let w = ui.available_width() - 98.0 - tag.chars().count() as f32 * 6.2;
            self.text_box(ui, w.max(110.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.busy() {
                    if icons::button_tinted(ui, Icon::Stop, AMBER, t("Stop (Esc)")).clicked() {
                        self.stop();
                    }
                } else if icons::button(ui, Icon::Send, t("Speak (Enter)")).clicked() {
                    let t = std::mem::take(&mut self.text);
                    self.speak(t);
                    self.focus_text = true;
                }
                if icons::button(ui, Icon::ChevronUp, t("Expand")).clicked() {
                    self.expand(true);
                }
                self.snap_widget(ui);
                let col = if self.error.is_some() { RED } else { ui.visuals().weak_text_color() };
                let r = ui.label(RichText::new(tag).small().color(col));
                if let Some(e) = &self.error {
                    r.on_hover_text(e.text());
                }
            });
        });
    }

    /// Debug aid: `CORVOX_SHOT=out.png` saves a screenshot after a moment, then quits.
    fn debug_screenshot(&mut self, ctx: &egui::Context) {
        let Ok(path) = std::env::var("CORVOX_SHOT") else { return };
        self.frames += 1;
        let main_ready = self.helpers.get(&self.main_key).map(|h| h.ready).unwrap_or(false);
        let waiting = std::env::var("CORVOX_SAY").is_ok() && (self.busy() || self.queued.is_some() || !main_ready);
        if waiting && self.frames < 2400 {
            self.frames = self.frames.min(38);
        }
        let wait_frames = std::env::var("CORVOX_SHOT_AFTER").ok().and_then(|s| s.parse::<u32>().ok()).map(|secs| secs * 20).unwrap_or(40);
        // Request again every 20 frames: a request can be dropped when egui discards a frame
        // to redo its layout.
        if self.frames < wait_frames || (self.frames - wait_frames) % 20 != 0 {
            ctx.request_repaint_after(Duration::from_millis(50));
        } else {
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

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_events();
        self.tend_spare();
        if self.spare_key.is_some() {
            ctx.request_repaint_after(Duration::from_secs(5));
        }
        self.update_ptt();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.debug_screenshot(ui.ctx());
        if self.snap_pending {
            self.snap_pending = false;
            if let Some(c) = self.cfg.snap {
                let size = if self.cfg.collapsed { COLLAPSED } else { EXPANDED };
                self.snap_to(c, size);
            }
        }
        let starting = self.helpers.values().any(|h| !h.ready);
        let active = self.busy()
            || !self.installs.is_empty()
            || !self.benching.is_empty()
            || self.ptt.is_held()
            || !self.model_dl.is_empty()
            || self.helpers.get(&self.main_key).map(|h| h.loading.is_some()).unwrap_or(false);
        if active || starting {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        let collapsed = self.cfg.collapsed;
        let visuals = ui.visuals().clone();
        let frame = egui::Frame::new()
            .fill(visuals.panel_fill)
            .stroke(visuals.widgets.noninteractive.bg_stroke)
            .corner_radius(CornerRadius::same(if platform::native_corners() { 0 } else if collapsed { 28 } else { 10 }))
            .inner_margin(if collapsed { egui::Margin::symmetric(10, 8) } else { egui::Margin::same(10) });
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
        self.helpers.clear();
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

fn vmic_discord_name() -> &'static str {
    if cfg!(windows) {
        "CABLE Output"
    } else if cfg!(target_os = "macos") {
        "BlackHole 2ch"
    } else {
        "Corvox Mic"
    }
}

/// Stable 64-bit FNV-1a, used to name saved phrase audio.
fn fnv(s: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
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
