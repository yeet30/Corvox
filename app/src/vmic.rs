//! "Set up virtual mic": gets a virtual audio cable onto the machine so Discord can use
//! Corvox as a microphone.
//!
//! - Windows: downloads VB-Cable (free, by VB-Audio) from vb-audio.com, checks the installer is
//!   signed by its author, and launches it elevated. The user approves the Windows prompt and
//!   clicks "Install Driver" in VB-Audio's own installer (which shows its license).
//!   We never redistribute VB-Cable.
//! - Linux: creates a PipeWire/PulseAudio null sink plus a mic that listens to it (no install).
//! - macOS: there's no unattended option for BlackHole, so the UI opens its download page.

#[cfg(windows)]
use std::io::{BufRead, BufReader};
#[cfg(not(target_os = "macos"))]
use std::process::Command;
#[cfg(windows)]
use std::process::Stdio;

use crate::engine::{Event, Tx};

pub const BLACKHOLE_URL: &str = "https://existential.audio/blackhole/";

fn send(tx: &Tx, e: Event) {
    let _ = tx.0.send(e);
    tx.1.request_repaint();
}

#[cfg(windows)]
const WINDOWS_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$dir = $env:CORVOX_DL_DIR
New-Item -ItemType Directory -Force $dir | Out-Null
$urls = @(
  'https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack46.zip',
  'https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack43.zip'
)
$zip = Join-Path $dir 'vbcable.zip'
$ok = $false
foreach ($u in $urls) {
  try {
    Write-Output ("STEP Downloading VB-Cable from vb-audio.com (" + [IO.Path]::GetFileName($u) + ")")
    Invoke-WebRequest -UseBasicParsing -Uri $u -OutFile $zip
    $ok = $true
    break
  } catch {
    Write-Output ("STEP Download failed: " + $_.Exception.Message)
  }
}
if (-not $ok) { exit 4 }
$x = Join-Path $dir 'files'
if (Test-Path $x) { Remove-Item -Recurse -Force $x }
Expand-Archive -Force $zip $x
$name = if ([Environment]::Is64BitOperatingSystem) { 'VBCABLE_Setup_x64.exe' } else { 'VBCABLE_Setup.exe' }
$exe = Join-Path $x $name
$sig = Get-AuthenticodeSignature $exe
if ($sig.Status -ne 'Valid' -or $sig.SignerCertificate.Subject -notmatch 'BUREL VINCENT') {
  Write-Output ("STEP The installer's signature didn't check out (" + $sig.Status + ")")
  exit 3
}
Write-Output "STEP Approve the Windows prompt, then click Install Driver"
try { Start-Process -FilePath $exe -Verb RunAs -Wait } catch { exit 2 }
exit 0
"#;

/// Runs the setup in the background; reports `VmicLine` updates and a final `VmicDone`.
pub fn setup(tx: Tx) {
    std::thread::spawn(move || {
        let code = run(&tx);
        send(&tx, Event::VmicDone { code });
    });
}

#[cfg(windows)]
fn run(tx: &Tx) -> i32 {
    use std::os::windows::process::CommandExt;
    let dir = crate::config::data_dir().join("downloads").join("vbcable");
    let mut c = Command::new("powershell");
    c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", WINDOWS_SCRIPT])
        .env("CORVOX_DL_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => {
            send(tx, Event::VmicLine(format!("Couldn't start PowerShell: {e}")));
            return 1;
        }
    };
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if let Some(step) = line.strip_prefix("STEP ") {
                send(tx, Event::VmicLine(step.trim().to_string()));
            }
        }
    }
    child.wait().ok().and_then(|s| s.code()).unwrap_or(1)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn run(tx: &Tx) -> i32 {
    let has_sink = Command::new("pactl")
        .args(["list", "short", "sinks"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("corvox"))
        .unwrap_or(false);
    if has_sink {
        return 0;
    }
    send(tx, Event::VmicLine("Creating the Corvox virtual mic".into()));
    let steps: [&[&str]; 2] = [
        &["load-module", "module-null-sink", "sink_name=corvox", "sink_properties=device.description=Corvox"],
        &[
            "load-module",
            "module-remap-source",
            "master=corvox.monitor",
            "source_name=corvox_mic",
            "source_properties=device.description=\"Corvox Mic\"",
        ],
    ];
    for args in steps {
        match Command::new("pactl").args(args).status() {
            Ok(s) if s.success() => {}
            Ok(_) | Err(_) => {
                send(tx, Event::VmicLine("pactl failed. Is PipeWire or PulseAudio running?".into()));
                return 1;
            }
        }
    }
    0
}

#[cfg(target_os = "macos")]
fn run(_tx: &Tx) -> i32 {
    5 // handled in the UI by opening BLACKHOLE_URL
}

/// Linux null sinks don't survive a reboot; recreate it quietly if the user set it up before.
pub fn restore_on_startup(tx: Tx) {
    if cfg!(all(unix, not(target_os = "macos"))) {
        std::thread::spawn(move || {
            let _ = run(&tx);
        });
    }
}
