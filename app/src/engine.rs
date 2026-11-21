//! Talks to the Python engine helper (engines/core) through `uv`.
//!
//! - `install`: `uv sync` for an engine's environment, streaming its output.
//! - `Helper`: a long-running `python -m corvox_engine` that loads models and speaks.
//! - `bench`: a one-shot `python -m corvox_engine --bench` in a fresh process.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use crossbeam_channel::Sender;
use serde_json::{json, Value};

use crate::config::{envs_dir, BenchResult};

#[derive(Debug)]
pub enum Event {
    /// `key` names one helper process: "main" speaks, "dl:<engine>" downloads a model in the background.
    HelperReady { key: String },
    HelperExited { key: String, msg: String },
    HelperStatus { key: String, msg: String },
    Status(String),
    Chunk { req: u64, samples: Vec<f32>, sr: u32 },
    SpeakDone { req: u64, result: Result<(), String> },
    LoadDone { key: String, result: Result<String, String> },
    InstallLine { env: String, line: String },
    /// Megabytes unpacked into uv's cache so far (a good proxy for download progress).
    InstallProgress { env: String, mb: f32 },
    InstallDone { env: String, ok: bool, cancelled: bool },
    ModelProgress { key: String, done_mb: f32 },
    BenchStatus { model: String, msg: String },
    BenchDone { model: String, result: Result<BenchResult, String> },
    Hotkey,
    VmicLine(String),
    VmicDone { code: i32 },
    DeleteDone { engine: String, freed_mb: f32, error: Option<String> },
    CacheCleaned { result: Result<String, String> },
}

pub type Tx = (Sender<Event>, eframe::egui::Context);

fn send(tx: &Tx, e: Event) {
    let _ = tx.0.send(e);
    tx.1.request_repaint();
}

fn env_dir(env: &str) -> PathBuf {
    envs_dir().join(env)
}

const MARKER: &str = ".corvox-installed";

pub fn is_installed(env: &str) -> bool {
    env_dir(env).join(MARKER).is_file()
}

fn base_command(uv: &Path, engines: &Path, env: &str) -> Command {
    let mut c = Command::new(uv);
    c.env("UV_PROJECT_ENVIRONMENT", env_dir(env))
        .env("PYTHONUNBUFFERED", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .current_dir(engines.join(env));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

fn env_python(env: &str) -> PathBuf {
    let dir = env_dir(env);
    if cfg!(windows) {
        dir.join("Scripts").join("python.exe")
    } else {
        dir.join("bin").join("python")
    }
}

/// Runs the engine helper with the environment's own Python. Going through `uv run` meant
/// stopping the helper only killed `uv`, and its Python child kept running (and downloading).
fn python_command(uv: &Path, engines: &Path, env: &str, agreed: bool, int8: bool) -> Command {
    let py = env_python(env);
    let mut c = if py.is_file() {
        let mut c = Command::new(&py);
        c.env("VIRTUAL_ENV", env_dir(env))
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .current_dir(engines.join(env));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        c
    } else {
        let mut c = base_command(uv, engines, env);
        c.args(["run", "--no-sync", "--project", "."]).arg("python");
        c
    };
    c.args(["-m", "corvox_engine"]);
    c.env("CORVOX_QUANTIZE", if int8 { "1" } else { "0" }).env("HF_HUB_DISABLE_SYMLINKS_WARNING", "1");
    if agreed {
        c.env("COQUI_TOS_AGREED", "1");
    }
    c
}

/// A process and everything it starts (on Windows a venv's python.exe is a launcher that
/// starts the real Python as a child). Dropping the group kills the whole tree, and so does
/// Corvox exiting or crashing, since the OS closes the handle then.
pub struct ProcGroup(#[cfg(windows)] isize);

#[cfg(windows)]
impl ProcGroup {
    pub fn bind(child: &Child) -> Option<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as *const _, std::mem::size_of_val(&info) as u32);
            AssignProcessToJobObject(job, child.as_raw_handle() as _);
            Some(Self(job as isize))
        }
    }
}

#[cfg(windows)]
impl Drop for ProcGroup {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0 as _);
        }
    }
}

#[cfg(not(windows))]
impl ProcGroup {
    pub fn bind(_child: &Child) -> Option<Self> {
        Some(Self())
    }
}

fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

/// uv streams each package into `<cache>/.tmpXXXX/` while downloading, then moves it into
/// `<cache>/archive-v0/`. Summing what appeared there since `since` tracks download progress.
fn uv_cache_new_bytes(cache: &Path, since: std::time::SystemTime) -> u64 {
    let mut total = 0;
    let Ok(rd) = std::fs::read_dir(cache) else { return 0 };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with(".tmp") {
            total += dir_size(&e.path());
        }
    }
    if let Ok(rd) = std::fs::read_dir(cache.join("archive-v0")) {
        for e in rd.flatten() {
            let fresh = e.metadata().and_then(|m| m.modified()).map(|t| t >= since).unwrap_or(false);
            if fresh {
                total += dir_size(&e.path());
            }
        }
    }
    total
}

pub type CancelHandle = Arc<Mutex<Option<Child>>>;

/// Runs `uv sync` for an engine environment in the background. Returns a handle that can kill it.
pub fn install(uv: PathBuf, engines: PathBuf, env: String, tx: Tx) -> CancelHandle {
    let handle: CancelHandle = Arc::new(Mutex::new(None));
    let h2 = handle.clone();
    std::thread::spawn(move || {
        let handle = h2;
        let cache = Command::new(&uv)
            .args(["cache", "dir"])
            .output()
            .ok()
            .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim().to_string()));
        let started = std::time::SystemTime::now() - std::time::Duration::from_secs(2);
        let mut c = base_command(&uv, &engines, &env);
        c.args(["sync", "--extra", "bench", "--project", "."]).stdout(Stdio::piped()).stderr(Stdio::piped());
        if engines.join(&env).join("uv.lock").is_file() {
            c.arg("--frozen");
        }
        let mut child = match c.spawn() {
            Ok(c) => c,
            Err(e) => {
                send(&tx, Event::InstallLine { env: env.clone(), line: format!("couldn't start uv: {e}") });
                send(&tx, Event::InstallDone { env, ok: false, cancelled: false });
                return;
            }
        };
        let _group = ProcGroup::bind(&child);
        let err = child.stderr.take().unwrap();
        let out = child.stdout.take();
        *handle.lock().unwrap() = Some(child);
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        if let Some(cache) = cache {
            let (tx3, env3, done3) = (tx.clone(), env.clone(), done.clone());
            std::thread::spawn(move || {
                while !done3.load(std::sync::atomic::Ordering::Relaxed) {
                    let mb = uv_cache_new_bytes(&cache, started) as f32 / 1_048_576.0;
                    send(&tx3, Event::InstallProgress { env: env3.clone(), mb });
                    std::thread::sleep(std::time::Duration::from_millis(1000));
                }
            });
        }
        let tx2 = tx.clone();
        let env2 = env.clone();
        let t = std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                send(&tx2, Event::InstallLine { env: env2.clone(), line });
            }
        });
        let _ = out.map(|o| BufReader::new(o).lines().count());
        let _ = t.join();
        let status = handle.lock().unwrap().take().map(|mut c| c.wait());
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        let cancelled = status.is_none();
        let ok = matches!(status, Some(Ok(s)) if s.success());
        if ok {
            let _ = std::fs::write(env_dir(&env).join(MARKER), "ok");
        }
        send(&tx, Event::InstallDone { env, ok, cancelled });
    });
    handle
}

/// Kills a running install. The environment stays half-installed; installing again resumes.
pub fn cancel_install(h: &CancelHandle) {
    if let Some(mut c) = h.lock().unwrap().take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// Deletes an engine's Python environment (if `remove_env`) and its model repos, including
/// half-finished downloads. Runs in the background; reports how much space was freed.
pub fn delete_engine_files(engine: String, env: Option<String>, repos: Vec<String>, tx: Tx) {
    std::thread::spawn(move || {
        // Give stopped helpers a moment to exit and release their files.
        std::thread::sleep(std::time::Duration::from_millis(600));
        let mut dirs: Vec<PathBuf> = repos
            .iter()
            .map(|r| crate::config::hf_hub_dir().join(format!("models--{}", r.replace('/', "--"))))
            .collect();
        if let Some(env) = &env {
            dirs.push(env_dir(env));
        }
        let mut freed = 0u64;
        let mut error = None;
        for d in dirs.iter().filter(|d| d.exists()) {
            let size = dir_size(d);
            let mut last = Ok(());
            for _ in 0..5 {
                last = std::fs::remove_dir_all(d);
                if last.is_ok() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            match last {
                Ok(()) => freed += size,
                Err(e) => error = Some(format!("{}: {e}", d.display())),
            }
        }
        send(&tx, Event::DeleteDone { engine, freed_mb: freed as f32 / 1_048_576.0, error });
    });
}

/// `uv cache clean`: removes uv's download cache. Installed engines keep working.
pub fn clean_uv_cache(uv: PathBuf, tx: Tx) {
    std::thread::spawn(move || {
        let mut c = Command::new(&uv);
        c.args(["cache", "clean"]).stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000);
        }
        let result = match c.output() {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stderr).to_string() + &String::from_utf8_lossy(&o.stdout);
                Ok(text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string())
            }
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("uv cache clean failed").to_string()),
            Err(e) => Err(e.to_string()),
        };
        send(&tx, Event::CacheCleaned { result });
    });
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Load,
    Speak,
    Other,
}

/// Request ids are unique across all helper processes, so audio from one never lands in another.
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A running engine helper process for one environment.
pub struct Helper {
    pub env: String,
    child: Child,
    group: Option<ProcGroup>,
    stdin: ChildStdin,
    pending: Arc<Mutex<HashMap<u64, (Kind, String)>>>,
}

impl Helper {
    pub fn spawn(uv: &Path, engines: &Path, env: &str, key: &str, agreed: bool, int8: bool, tx: Tx) -> anyhow::Result<Self> {
        let mut c = python_command(uv, engines, env, agreed, int8);
        c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn()?;
        let group = ProcGroup::bind(&child);
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let pending: Arc<Mutex<HashMap<u64, (Kind, String)>>> = Default::default();

        // stderr: keep the last lines so we can explain a crash.
        let tail = Arc::new(Mutex::new(Vec::<String>::new()));
        let tail2 = tail.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("[engine] {line}");
                let mut t = tail2.lock().unwrap();
                t.push(line);
                let n = t.len();
                if n > 30 {
                    t.drain(..n - 30);
                }
            }
        });

        let key_name = key.to_string();
        let pend = pending.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                handle_line(&v, &key_name, &pend, &tx);
            }
            let last = tail.lock().unwrap().iter().rev().find(|l| !l.trim().is_empty()).cloned().unwrap_or_default();
            for (id, (kind, _model)) in pend.lock().unwrap().drain() {
                let err = format!("engine stopped: {last}");
                match kind {
                    Kind::Speak => send(&tx, Event::SpeakDone { req: id, result: Err(err) }),
                    Kind::Load => send(&tx, Event::LoadDone { key: key_name.clone(), result: Err(err) }),
                    Kind::Other => {}
                }
            }
            send(&tx, Event::HelperExited { key: key_name, msg: last });
        });

        Ok(Self { env: env.to_string(), child, group, stdin, pending })
    }

    fn request(&mut self, kind: Kind, model: &str, mut body: Value) -> anyhow::Result<u64> {
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        body["id"] = json!(id);
        self.pending.lock().unwrap().insert(id, (kind, model.to_string()));
        writeln!(self.stdin, "{body}")?;
        self.stdin.flush()?;
        Ok(id)
    }

    pub fn load(&mut self, model: &str, language: &str, voice: Option<&Path>, model_mb: u32) -> anyhow::Result<u64> {
        self.request(
            Kind::Load,
            model,
            json!({"cmd": "load", "model": model, "language": language, "model_mb": model_mb,
                   "voice": voice.map(|v| v.to_string_lossy().to_string())}),
        )
    }

    /// Stops the current speech right away (and drops queued ones).
    pub fn cancel(&mut self) {
        let _ = writeln!(self.stdin, "{}", json!({"cmd": "cancel"}));
        let _ = self.stdin.flush();
    }

    pub fn speak(&mut self, model: &str, text: &str, voice: &Path, language: &str, model_mb: u32) -> anyhow::Result<u64> {
        self.request(
            Kind::Speak,
            model,
            json!({"cmd": "speak", "model": model, "text": text, "voice": voice.to_string_lossy(), "language": language, "model_mb": model_mb}),
        )
    }

    pub fn unload(&mut self, model: &str) {
        let _ = self.request(Kind::Other, model, json!({"cmd": "unload", "model": model}));
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "{}", json!({"cmd": "quit", "id": 0}));
        let _ = self.stdin.flush();
        self.group = None; // closing the job kills the launcher and the real Python under it
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn decode_pcm16(b64: &str) -> Vec<f32> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).unwrap_or_default();
    bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect()
}

fn handle_line(v: &Value, key: &str, pending: &Arc<Mutex<HashMap<u64, (Kind, String)>>>, tx: &Tx) {
    if let Some(ev) = v.get("event").and_then(|e| e.as_str()) {
        match ev {
            "ready" => send(tx, Event::HelperReady { key: key.to_string() }),
            "status" => send(tx, Event::HelperStatus { key: key.to_string(), msg: v["msg"].as_str().unwrap_or("").to_string() }),
            "progress" => {
                let id = v["id"].as_u64().unwrap_or(0);
                if pending.lock().unwrap().contains_key(&id) {
                    send(tx, Event::ModelProgress { key: key.to_string(), done_mb: v["done_mb"].as_f64().unwrap_or(0.0) as f32 });
                }
            }
            "chunk" => send(
                tx,
                Event::Chunk {
                    req: v["id"].as_u64().unwrap_or(0),
                    samples: decode_pcm16(v["pcm16"].as_str().unwrap_or("")),
                    sr: v["sample_rate"].as_u64().unwrap_or(24000) as u32,
                },
            ),
            _ => {}
        }
        return;
    }
    let Some(id) = v.get("id").and_then(|i| i.as_u64()) else { return };
    let Some((kind, _model)) = pending.lock().unwrap().remove(&id) else { return };
    let ok = v["ok"].as_bool().unwrap_or(false);
    let err = v["error"].as_str().unwrap_or("unknown error").to_string();
    match kind {
        Kind::Speak => send(tx, Event::SpeakDone { req: id, result: if ok { Ok(()) } else { Err(err) } }),
        Kind::Load => send(
            tx,
            Event::LoadDone {
                key: key.to_string(),
                result: if ok { Ok(v["device"].as_str().unwrap_or("cpu").to_string()) } else { Err(err) },
            },
        ),
        Kind::Other => {}
    }
}

/// Runs "Test on my PC" for one engine in a fresh helper process.
#[allow(clippy::too_many_arguments)]
pub fn bench(uv: PathBuf, engines: PathBuf, env: String, model: String, voice: PathBuf, language: String, agreed: bool, int8: bool, tx: Tx) {
    std::thread::spawn(move || {
        let mut c = python_command(&uv, &engines, &env, agreed, int8);
        c.args(["--bench", "--model", &model, "--language", &language, "--voice"])
            .arg(&voice)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match c.spawn() {
            Ok(c) => c,
            Err(e) => {
                send(&tx, Event::BenchDone { model, result: Err(format!("couldn't start: {e}")) });
                return;
            }
        };
        let _group = ProcGroup::bind(&child);
        let stderr = child.stderr.take().unwrap();
        let tail = std::thread::spawn(move || {
            let mut last = String::new();
            for l in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("[bench] {l}");
                if !l.trim().is_empty() {
                    last = l;
                }
            }
            last
        });
        let mut result: Option<Result<BenchResult, String>> = None;
        for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            match v["event"].as_str() {
                Some("status") => send(&tx, Event::BenchStatus { model: model.clone(), msg: v["msg"].as_str().unwrap_or("").into() }),
                Some("bench_result") => {
                    result = Some(if v["ok"].as_bool() == Some(true) {
                        let f = |k: &str| v[k].as_f64().map(|x| x as f32);
                        Ok(BenchResult {
                            speed: f("speed"),
                            ram_mb: f("ram_mb"),
                            vram_mb: f("vram_mb"),
                            voice_match: f("voice_match"),
                            clarity: f("clarity"),
                            language: language.clone(),
                            int8: v["int8"].as_bool().unwrap_or(false),
                            voice: voice.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
                        })
                    } else {
                        Err(v["error"].as_str().unwrap_or("failed").to_string())
                    })
                }
                _ => {}
            }
        }
        let _ = child.wait();
        let last = tail.join().unwrap_or_default();
        send(&tx, Event::BenchDone { model, result: result.unwrap_or_else(|| Err(format!("test crashed: {last}"))) });
    });
}
