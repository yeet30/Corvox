use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Numbers from "Test on my PC", stored per engine.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BenchResult {
    pub speed: Option<f32>,
    pub ram_mb: Option<f32>,
    pub vram_mb: Option<f32>,
    pub voice_match: Option<f32>,
    pub clarity: Option<f32>,
    pub language: String,
    pub voice: String,
    #[serde(default)]
    pub int8: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub language: String,
    pub engine: String,
    pub voice: Option<String>,
    pub recent: Vec<String>,
    pub pinned: Vec<String>,
    /// Name of the output device that feeds Discord (e.g. "CABLE Input"). None = auto-detect.
    pub output_device: Option<String>,
    /// Also play to the default speakers/headphones so you hear yourself.
    pub monitor: bool,
    pub monitor_volume: f32,
    /// Key held while speaking, e.g. "F13". None = off.
    pub ptt_key: Option<String>,
    /// Global shortcut that brings the window up, e.g. "Ctrl+Shift+Space".
    pub hotkey: String,
    pub collapsed: bool,
    pub always_on_top: bool,
    pub bench: HashMap<String, BenchResult>,
    /// Engines whose license the user agreed to (XTTS-v2).
    pub agreed: Vec<String>,
    /// The user ran "Set up virtual mic" (Linux recreates its sink on startup).
    pub vmic_setup: bool,
    /// int8 quantization for the Pocket engines: ~10% faster, ~450 MB less RAM.
    pub int8: bool,
    /// UI language: "en" or "tr".
    pub ui_lang: String,
    /// Remember recent phrases and keep their audio so they replay instantly.
    pub keep_phrases: bool,
    /// How many recent phrases to keep (older ones and their audio are deleted).
    pub phrase_limit: usize,
    /// Models already downloaded, as model keys ("xtts", "pocket-tr", "pocket:fr", ...).
    pub downloaded: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: "tr".into(),
            engine: "pocket-tr".into(),
            voice: None,
            recent: vec![],
            pinned: vec![],
            output_device: None,
            monitor: true,
            monitor_volume: 0.6,
            ptt_key: None,
            hotkey: "Ctrl+Shift+Space".into(),
            collapsed: false,
            always_on_top: true,
            bench: HashMap::new(),
            agreed: vec![],
            vmic_setup: false,
            int8: true,
            ui_lang: if sys_lang_is_turkish() { "tr".into() } else { "en".into() },
            keep_phrases: true,
            phrase_limit: 8,
            downloaded: vec![],
        }
    }
}

pub const MAX_PHRASE_LIMIT: usize = 50;

fn sys_lang_is_turkish() -> bool {
    ["LANG", "LC_ALL", "LANGUAGE"].iter().any(|k| std::env::var(k).map(|v| v.to_lowercase().starts_with("tr")).unwrap_or(false))
}

pub fn dirs() -> directories::ProjectDirs {
    directories::ProjectDirs::from("", "", "Corvox").expect("no home directory")
}

pub fn data_dir() -> PathBuf {
    let d = dirs().data_dir().to_path_buf();
    let _ = std::fs::create_dir_all(&d);
    d
}

/// `CORVOX_HOME` moves settings and voices elsewhere (handy for testing). Engines stay put.
fn home_override() -> Option<PathBuf> {
    std::env::var_os("CORVOX_HOME").map(PathBuf::from)
}

pub fn voices_dir() -> PathBuf {
    let d = home_override().unwrap_or_else(data_dir).join("voices");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn envs_dir() -> PathBuf {
    // CORVOX_ENVS moves the engine environments (handy for testing without touching real installs).
    let d = std::env::var_os("CORVOX_ENVS").map(PathBuf::from).unwrap_or_else(|| data_dir().join("envs"));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn config_path() -> PathBuf {
    let d = home_override().unwrap_or_else(|| dirs().config_dir().to_path_buf());
    let _ = std::fs::create_dir_all(&d);
    d.join("config.json")
}

impl Config {
    pub fn load() -> Self {
        std::fs::read_to_string(config_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(config_path(), s);
        }
    }

    /// Adds a phrase to the top of the recent list and returns the phrases that fell off the end.
    pub fn push_recent(&mut self, text: &str) -> Vec<String> {
        let text = text.trim();
        if text.is_empty() || !self.keep_phrases {
            return vec![];
        }
        self.recent.retain(|t| t != text);
        self.recent.insert(0, text.to_string());
        self.trim_recent()
    }

    /// Applies `phrase_limit`, returning what was removed. Pinned phrases don't count toward it.
    pub fn trim_recent(&mut self) -> Vec<String> {
        let limit = self.phrase_limit.clamp(1, MAX_PHRASE_LIMIT);
        let pinned = self.pinned.clone();
        let mut kept = 0;
        let mut removed = vec![];
        self.recent.retain(|t| {
            if pinned.contains(t) {
                return true;
            }
            kept += 1;
            if kept <= limit {
                true
            } else {
                removed.push(t.clone());
                false
            }
        });
        removed
    }

    pub fn is_downloaded(&self, key: &str) -> bool {
        self.downloaded.iter().any(|d| d == key)
    }

    pub fn forget_downloaded(&mut self, pred: impl Fn(&str) -> bool) {
        self.downloaded.retain(|d| !pred(d));
    }

    pub fn mark_downloaded(&mut self, key: &str) {
        if !self.is_downloaded(key) {
            self.downloaded.push(key.to_string());
        }
    }

    pub fn toggle_pin(&mut self, text: &str) {
        if let Some(i) = self.pinned.iter().position(|t| t == text) {
            self.pinned.remove(i);
        } else {
            self.pinned.push(text.to_string());
        }
    }

    pub fn voice_path(&self) -> Option<PathBuf> {
        let v = self.voice.as_ref()?;
        let p = voices_dir().join(format!("{v}.wav"));
        p.exists().then_some(p)
    }
}

/// Hugging Face's model cache (respects HF_HUB_CACHE / HF_HOME like huggingface_hub does).
pub fn hf_hub_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("HF_HUB_CACHE") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HF_HOME").map(PathBuf::from).unwrap_or_else(|| {
        directories::BaseDirs::new().map(|b| b.home_dir().join(".cache").join("huggingface")).unwrap_or_default()
    });
    home.join("hub")
}

/// Where replayable audio of recent phrases is kept.
pub fn phrase_cache_dir() -> PathBuf {
    let d = home_override().unwrap_or_else(data_dir).join("phrase-audio");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn list_voices() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(voices_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension()?.to_str()? == "wav").then(|| p.file_stem()?.to_str().map(String::from))?
        })
        .collect();
    v.sort();
    v
}

/// Where huggingface_hub keeps its login token (same place `hf auth login` writes).
pub fn hf_token_path() -> PathBuf {
    if let Ok(p) = std::env::var("HF_TOKEN_PATH") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HF_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        directories::BaseDirs::new().map(|b| b.home_dir().join(".cache").join("huggingface")).unwrap_or_default()
    });
    home.join("token")
}

pub fn has_hf_token() -> bool {
    std::env::var("HF_TOKEN").is_ok() || std::fs::read_to_string(hf_token_path()).map(|s| !s.trim().is_empty()).unwrap_or(false)
}

pub fn save_hf_token(token: &str) -> std::io::Result<()> {
    let p = hf_token_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(p, token.trim())
}
