use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Published {
    pub speed: Option<f32>,
    pub ram_mb: Option<f32>,
    pub vram_mb: Option<f32>,
    pub voice_match: Option<f32>,
    pub clarity: Option<f32>,
    #[serde(default)]
    pub source: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Engine {
    pub id: String,
    pub name: String,
    pub by: String,
    pub env: String,
    pub device: String,
    pub languages: Vec<String>,
    /// Unpacked size of the Python environment (shared by engines with the same `env`).
    pub env_mb: u32,
    /// Size of the model files downloaded on first use.
    pub model_mb: u32,
    #[serde(default)]
    pub published: Published,
    pub license: String,
    #[serde(default)]
    pub requires_agreement: bool,
    #[serde(default)]
    pub requires_hf: bool,
    pub url: String,
    pub note: String,
}

impl Engine {
    pub fn is_gpu(&self) -> bool {
        self.device == "gpu"
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Catalog {
    pub languages: BTreeMap<String, String>,
    pub engines: Vec<Engine>,
}

impl Catalog {
    pub fn load(engines_dir: &Path) -> anyhow::Result<Self> {
        let s = std::fs::read_to_string(engines_dir.join("catalog.json"))?;
        Ok(serde_json::from_str(&s)?)
    }

    pub fn engine(&self, id: &str) -> Option<&Engine> {
        self.engines.iter().find(|e| e.id == id)
    }

    pub fn lang_name<'a>(&'a self, code: &'a str) -> &'a str {
        self.languages.get(code).map(|s| s.as_str()).unwrap_or(code)
    }

    /// Languages that at least one engine speaks, sorted by display name.
    pub fn all_languages(&self) -> Vec<String> {
        let mut v: Vec<String> = self.languages.keys().filter(|c| self.engines.iter().any(|e| e.languages.contains(c))).cloned().collect();
        v.sort_by_key(|c| self.lang_name(c).to_string());
        v
    }

    pub fn engines_for(&self, lang: &str) -> Vec<&Engine> {
        self.engines.iter().filter(|e| e.languages.iter().any(|l| l == lang)).collect()
    }
}

/// Finds the `engines/` folder: $CORVOX_ENGINES, next to the exe, a few levels up
/// from the exe (when running from `app/target/release`), or the current directory.
pub fn find_engines_dir() -> Option<PathBuf> {
    let ok = |p: &Path| p.join("catalog.json").is_file();
    if let Ok(p) = std::env::var("CORVOX_ENGINES") {
        let p = PathBuf::from(p);
        if ok(&p) {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.ancestors().skip(1).take(5) {
            let p = dir.join("engines");
            if ok(&p) {
                return Some(p);
            }
        }
    }
    let p = std::env::current_dir().ok()?.join("engines");
    ok(&p).then_some(p)
}

/// Finds the `uv` executable: on PATH, or in the usual install locations.
pub fn find_uv() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "uv.exe" } else { "uv" };
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join(exe);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
    let mut candidates = vec![home.join(".local/bin").join(exe), home.join(".cargo/bin").join(exe)];
    if cfg!(windows) {
        let local = home.join("AppData/Local/Microsoft/WinGet");
        candidates.push(local.join("Links").join(exe));
        if let Ok(rd) = std::fs::read_dir(local.join("Packages")) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with("astral-sh.uv") {
                    candidates.push(e.path().join(exe));
                }
            }
        }
    } else {
        candidates.push(PathBuf::from("/opt/homebrew/bin/uv"));
        candidates.push(PathBuf::from("/usr/local/bin/uv"));
    }
    candidates.into_iter().find(|p| p.is_file())
}
