//! Updates from GitHub Releases. Corvox asks GitHub for the latest release, downloads the zip
//! for this OS and swaps it in next to the running app, then restarts. The zips are built by
//! `.github/workflows/release.yml` whenever a `v*` tag is pushed.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::engine::{send, Event, Tx};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// "owner/name" of the GitHub repo updates come from. Release builds set `CORVOX_REPO` to the
/// repo they were built in, so a fork updates from its own releases.
pub fn repo() -> &'static str {
    match option_env!("CORVOX_REPO") {
        Some(r) if !r.is_empty() => r,
        _ => "yeet30/Corvox",
    }
}

pub fn releases_page() -> String {
    format!("https://github.com/{}/releases/latest", repo())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub page: String,
    /// This OS's zip, if the release has one.
    pub asset: Option<Asset>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// End of the release zip's name for this OS (`corvox-<version>-<suffix>`), as the release
/// workflow names them.
pub fn asset_suffix() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("windows-x64.zip")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("macos-arm64.zip")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux-x64.zip")
    } else {
        None
    }
}

fn numbers(v: &str) -> Vec<u64> {
    let core = v.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next().unwrap_or("");
    core.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// Whether version `a` is newer than `b` ("v0.2.0" > "0.1.9"; missing parts count as 0).
pub fn is_newer(a: &str, b: &str) -> bool {
    let (a, b) = (numbers(a), numbers(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .user_agent("Corvox-updater")
        .build()
        .into()
}

/// Debug aid: `CORVOX_UPDATE_URL` replaces the GitHub API address (e.g. a local test server).
fn latest_url() -> String {
    std::env::var("CORVOX_UPDATE_URL").unwrap_or_else(|_| format!("https://api.github.com/repos/{}/releases/latest", repo()))
}

/// Looks for a newer release in the background. Sends `UpdateChecked` with `Some` only if
/// the latest release is newer than this build.
pub fn check(tx: Tx, manual: bool) {
    std::thread::spawn(move || {
        let result = fetch_latest().map(|r| r.filter(|r| is_newer(&r.version, VERSION)));
        send(&tx, Event::UpdateChecked { result, manual });
    });
}

fn fetch_latest() -> Result<Option<Release>, String> {
    let mut res = match agent().get(&latest_url()).header("Accept", "application/vnd.github+json").call() {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(404)) => return Ok(None), // no releases yet
        Err(e) => return Err(e.to_string()),
    };
    let body = res.body_mut().read_to_string().map_err(|e| e.to_string())?;
    parse_release(&body, asset_suffix())
}

fn parse_release(json: &str, suffix: Option<&str>) -> Result<Option<Release>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let Some(tag) = v["tag_name"].as_str() else { return Ok(None) };
    let asset = v["assets"].as_array().and_then(|list| {
        list.iter().find_map(|a| {
            let name = a["name"].as_str()?;
            if !suffix.is_some_and(|s| name.ends_with(s)) {
                return None;
            }
            Some(Asset { name: name.to_string(), url: a["browser_download_url"].as_str()?.to_string(), size: a["size"].as_u64().unwrap_or(0) })
        })
    });
    Ok(Some(Release {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        page: v["html_url"].as_str().map(String::from).unwrap_or_else(releases_page),
        asset,
    }))
}

static EXE: OnceLock<PathBuf> = OnceLock::new();

/// Remembers where the app was started from (renaming the running exe during an update can
/// change what `current_exe` reports later). Call first thing in `main`.
pub fn init() {
    if let Ok(p) = std::env::current_exe() {
        let _ = EXE.set(p);
    }
}

fn exe() -> Option<PathBuf> {
    EXE.get().cloned().or_else(|| std::env::current_exe().ok())
}

/// The folder the app runs from, if it's a release folder (the app with `engines/` beside it).
/// A build run from a source checkout never replaces itself.
pub fn install_dir() -> Option<PathBuf> {
    let dir = exe()?.parent()?.to_path_buf();
    dir.join("engines").join("catalog.json").is_file().then_some(dir)
}

/// Whether "Update and restart" can work here: a release folder we're allowed to write to.
pub fn can_self_update() -> bool {
    let Some(dir) = install_dir() else { return false };
    let probe = dir.join(".corvox-write-test");
    let ok = fs::write(&probe, b"").is_ok();
    let _ = fs::remove_file(&probe);
    ok
}

const STAGE: &str = ".corvox-update";
const OLD: &str = ".old";

/// Removes what the last update left behind: the previous version's files (`<name>.old`, kept
/// because a running exe can be renamed but not deleted) and the download folder.
pub fn cleanup() {
    let Some(dir) = install_dir() else { return };
    let _ = fs::remove_dir_all(dir.join(STAGE));
    let Ok(rd) = fs::read_dir(&dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if let Some(orig) = name.strip_suffix(OLD) {
            // Only backups of things that exist, never someone's own "*.old" file.
            if !orig.is_empty() && dir.join(orig).exists() {
                remove_any(&e.path());
            }
        }
    }
}

/// Downloads and installs `asset` in the background. Sends `UpdateProgress` while
/// downloading and `UpdateDone` at the end; after `Ok` the new version is in place and
/// [`restart`] starts it.
pub fn apply(asset: Asset, tx: Tx) {
    std::thread::spawn(move || {
        let result = install(&asset, &tx);
        send(&tx, Event::UpdateDone { result });
    });
}

fn install(asset: &Asset, tx: &Tx) -> Result<(), String> {
    let dir = install_dir().ok_or("Corvox isn't running from a release folder")?;
    let stage = dir.join(STAGE);
    let _ = fs::remove_dir_all(&stage);
    fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
    let zip = stage.join("download.zip");
    download(asset, &zip, tx)?;
    let unpacked = stage.join("new");
    extract(&zip, &unpacked)?;
    let root = top_folder(&unpacked);
    let exe_name = exe().and_then(|p| p.file_name().map(|n| n.to_owned())).ok_or("unknown app path")?;
    if !root.join(&exe_name).is_file() || !root.join("engines").join("catalog.json").is_file() {
        return Err("the download doesn't look like a Corvox release".into());
    }
    swap(&dir, &root)?;
    let _ = fs::remove_dir_all(&stage);
    Ok(())
}

fn download(asset: &Asset, to: &Path, tx: &Tx) -> Result<(), String> {
    let mut res = agent().get(&asset.url).call().map_err(|e| e.to_string())?;
    let total = res.body().content_length().unwrap_or(asset.size);
    let mut reader = res.body_mut().as_reader();
    let mut out = fs::File::create(to).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1 << 16];
    let mut done = 0u64;
    let mut last = Instant::now();
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if last.elapsed() >= Duration::from_millis(200) {
            last = Instant::now();
            send(tx, Event::UpdateProgress { done, total });
        }
    }
    out.flush().map_err(|e| e.to_string())?;
    if asset.size > 0 && done != asset.size {
        return Err(format!("download incomplete ({done} of {} bytes)", asset.size));
    }
    send(tx, Event::UpdateProgress { done, total: done });
    Ok(())
}

fn extract(zip: &Path, to: &Path) -> Result<(), String> {
    let f = fs::File::open(zip).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| e.to_string())?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        // `enclosed_name` drops entries that would land outside the folder ("../", absolute).
        let Some(rel) = entry.enclosed_name() else { continue };
        let out = to.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(p) = out.parent() {
            fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let mut w = fs::File::create(&out).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut w).map_err(|e| e.to_string())?; // checks the CRC too
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&out, fs::Permissions::from_mode(mode & 0o777));
        }
    }
    Ok(())
}

/// The zip's contents, skipping a single top-level folder ("Corvox/") if it has one.
fn top_folder(dir: &Path) -> PathBuf {
    let entries: Vec<_> = fs::read_dir(dir).map(|rd| rd.flatten().collect()).unwrap_or_default();
    // A release has several entries (the app, engines/, ...), so a lone folder is a wrapper.
    match entries.as_slice() {
        [only] if only.path().is_dir() => only.path(),
        _ => dir.to_path_buf(),
    }
}

fn remove_any(p: &Path) {
    if p.is_dir() {
        let _ = fs::remove_dir_all(p);
    } else {
        let _ = fs::remove_file(p);
    }
}

/// Retries for a few seconds: engine processes that were just stopped can hold their folder
/// a little longer.
fn retry(mut f: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match f() {
            Ok(()) => return Ok(()),
            Err(e) if tries >= 20 => return Err(e),
            Err(_) => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// Moves each top-level entry of `new` into `dir`, keeping what it replaces as `<name>.old`
/// until the next start. If any step fails, everything moved so far is put back.
fn swap(dir: &Path, new: &Path) -> Result<(), String> {
    let mut names: Vec<_> = fs::read_dir(new).map_err(|e| e.to_string())?.flatten().map(|e| e.file_name()).collect();
    names.sort();
    let mut done: Vec<(PathBuf, Option<PathBuf>)> = vec![];
    let undo = |done: &[(PathBuf, Option<PathBuf>)]| {
        for (target, backup) in done.iter().rev() {
            remove_any(target);
            if let Some(b) = backup {
                let _ = fs::rename(b, target);
            }
        }
    };
    for name in names {
        let label = name.to_string_lossy().to_string();
        let target = dir.join(&name);
        let backup = dir.join(format!("{label}{OLD}"));
        remove_any(&backup);
        let had = target.exists();
        if had {
            if let Err(e) = retry(|| fs::rename(&target, &backup)) {
                undo(&done);
                return Err(format!("couldn't move the old {label} aside: {e}"));
            }
        }
        if let Err(e) = retry(|| fs::rename(new.join(&name), &target)) {
            if had {
                let _ = fs::rename(&backup, &target);
            }
            undo(&done);
            return Err(format!("couldn't put the new {label} in place: {e}"));
        }
        done.push((target, had.then_some(backup)));
    }
    Ok(())
}

/// Starts the freshly installed version. It waits for this process to exit before it opens,
/// so the shortcut and the audio devices are free.
pub fn restart() -> Result<(), String> {
    let exe = exe().ok_or("unknown app path")?;
    std::process::Command::new(exe).arg("--wait-pid").arg(std::process::id().to_string()).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// `--wait-pid <pid>` on the command line (from [`restart`]): the old version's process.
pub fn wait_pid_arg() -> Option<u32> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == "--wait-pid").and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1.1"));
        assert!(!is_newer("0.1.0-beta", "0.1.0"));
    }

    #[test]
    fn picks_this_platforms_zip() {
        let json = r#"{"tag_name":"v0.3.1","html_url":"https://github.com/o/r/releases/tag/v0.3.1","assets":[
            {"name":"corvox-0.3.1-linux-x64.zip","browser_download_url":"https://x/l.zip","size":10},
            {"name":"corvox-0.3.1-windows-x64.zip","browser_download_url":"https://x/w.zip","size":20}]}"#;
        let r = parse_release(json, Some("windows-x64.zip")).unwrap().unwrap();
        assert_eq!(r.version, "0.3.1");
        assert_eq!(r.asset, Some(Asset { name: "corvox-0.3.1-windows-x64.zip".into(), url: "https://x/w.zip".into(), size: 20 }));
        assert_eq!(parse_release(json, Some("macos-arm64.zip")).unwrap().unwrap().asset, None);
    }

    #[test]
    fn skips_a_wrapper_folder() {
        let base = std::env::temp_dir().join(format!("corvox-top-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("Corvox").join("engines")).unwrap();
        fs::write(base.join("Corvox").join("corvox"), "").unwrap();
        assert_eq!(top_folder(&base), base.join("Corvox"));
        assert_eq!(top_folder(&base.join("Corvox")), base.join("Corvox"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn swap_moves_entries_and_keeps_backups() {
        let base = std::env::temp_dir().join(format!("corvox-swap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (dir, new) = (base.join("app"), base.join("new"));
        fs::create_dir_all(dir.join("engines")).unwrap();
        fs::write(dir.join("engines").join("a.txt"), "old").unwrap();
        fs::write(dir.join("corvox"), "old exe").unwrap();
        fs::create_dir_all(new.join("engines")).unwrap();
        fs::write(new.join("engines").join("a.txt"), "new").unwrap();
        fs::write(new.join("corvox"), "new exe").unwrap();
        fs::write(new.join("README.md"), "hi").unwrap();
        swap(&dir, &new).unwrap();
        assert_eq!(fs::read_to_string(dir.join("engines").join("a.txt")).unwrap(), "new");
        assert_eq!(fs::read_to_string(dir.join("corvox")).unwrap(), "new exe");
        assert_eq!(fs::read_to_string(dir.join("corvox.old")).unwrap(), "old exe");
        assert!(dir.join("engines.old").join("a.txt").is_file());
        assert!(dir.join("README.md").is_file());
        let _ = fs::remove_dir_all(&base);
    }
}
