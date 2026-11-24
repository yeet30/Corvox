//! Audio: importing voice clips, resampling, and playing speech to the virtual mic.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

pub const VOICE_SR: u32 = 24_000;
const MAX_CLIP_SECONDS: f32 = 20.0;

// ---------- decoding and preparing a voice clip ----------

/// Decodes any common audio file (mp3, wav, flac, ogg, m4a…) to mono f32.
pub fn decode_to_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let file = Box::new(std::fs::File::open(path).with_context(|| format!("can't open {}", path.display()))?);
    let mss = MediaSourceStream::new(file, Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| anyhow!("unsupported audio file: {e}"))?;
    let track = format.default_track(TrackType::Audio).ok_or_else(|| anyhow!("no audio track"))?;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or_else(|| anyhow!("no audio codec"))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| anyhow!("unsupported codec: {e}"))?;
    let track_id = track.id;

    let mut mono = Vec::new();
    let mut rate = 0u32;
    let mut buf: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(Error::IoError(_)) => break,
            Err(e) => return Err(anyhow!("read error: {e}")),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(ab) => {
                rate = ab.spec().rate();
                let ch = ab.spec().channels().count().max(1);
                buf.resize(ab.samples_interleaved(), 0.0);
                ab.copy_to_slice_interleaved(&mut buf);
                mono.extend(buf.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32));
                if mono.len() as f32 > rate as f32 * 120.0 {
                    break; // plenty; we only keep ~20 s anyway
                }
            }
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(anyhow!("decode error: {e}")),
        }
    }
    if mono.is_empty() || rate == 0 {
        return Err(anyhow!("the file has no audio"));
    }
    Ok((mono, rate))
}

/// Windowed-sinc resampler. Fine for short clips; band-limits when downsampling.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let cutoff = ratio.min(1.0) * 0.95;
    let half = 16.0 / cutoff.min(1.0);
    let out_len = (input.len() as f64 * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for n in 0..out_len {
        let center = n as f64 / ratio;
        let lo = (center - half).ceil().max(0.0) as usize;
        let hi = ((center + half).floor() as usize).min(input.len() - 1);
        let (mut acc, mut wsum) = (0.0f64, 0.0f64);
        for (i, &s) in input.iter().enumerate().take(hi + 1).skip(lo) {
            let x = i as f64 - center;
            let sinc = if x.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * x * cutoff).sin() / (std::f64::consts::PI * x * cutoff) };
            let w = 0.5 + 0.5 * (std::f64::consts::PI * x / half).cos(); // Hann window
            let k = sinc * w;
            acc += s as f64 * k;
            wsum += k;
        }
        out.push(if wsum.abs() > 1e-9 { (acc / wsum) as f32 } else { 0.0 });
    }
    out
}

/// Cuts leading/trailing silence, caps the length, and normalizes the peak to -1 dBFS.
pub fn tidy_clip(samples: &[f32], sr: u32) -> Vec<f32> {
    let win = (sr / 50) as usize; // 20 ms
    let rms: Vec<f32> = samples.chunks(win).map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt()).collect();
    let peak_rms = rms.iter().cloned().fold(0.0, f32::max);
    let thresh = peak_rms * 0.05;
    let first = rms.iter().position(|&r| r > thresh).unwrap_or(0);
    let last = rms.iter().rposition(|&r| r > thresh).unwrap_or(rms.len().saturating_sub(1));
    let pad = 5; // keep 100 ms around the speech
    let start = first.saturating_sub(pad) * win;
    let end = ((last + 1 + pad) * win).min(samples.len());
    let mut clip: Vec<f32> = samples[start..end.max(start)].to_vec();
    clip.truncate((MAX_CLIP_SECONDS * sr as f32) as usize);
    let peak = clip.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak > 1e-4 {
        let g = 0.89 / peak;
        clip.iter_mut().for_each(|s| *s *= g);
    }
    clip
}

pub fn write_wav(path: &Path, samples: &[f32], sr: u32) -> Result<()> {
    let spec = hound::WavSpec { channels: 1, sample_rate: sr, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    w.finalize()?;
    Ok(())
}

/// Turns any audio file into a clean 24 kHz mono WAV voice reference. Returns its length in seconds.
pub fn import_voice(src: &Path, dest: &Path) -> Result<f32> {
    let (mono, sr) = decode_to_mono(src)?;
    let clip = tidy_clip(&resample(&mono, sr, VOICE_SR), VOICE_SR);
    let secs = clip.len() as f32 / VOICE_SR as f32;
    if secs < 2.0 {
        return Err(anyhow!("the clip has less than 2 seconds of speech"));
    }
    write_wav(dest, &clip, VOICE_SR)?;
    Ok(secs)
}

pub fn wav_seconds(path: &Path) -> Option<f32> {
    let r = hound::WavReader::open(path).ok()?;
    Some(r.duration() as f32 / r.spec().sample_rate as f32)
}

/// A short, soft two-note chime for "engine ready".
pub fn chime() -> (Vec<f32>, u32) {
    let sr = 48_000u32;
    let len = (sr as f32 * 0.7) as usize;
    let mut out = vec![0.0f32; len];
    for (start, freq) in [(0.0f32, 987.8f32), (0.12, 1318.5)] {
        let s0 = (start * sr as f32) as usize;
        for (i, o) in out.iter_mut().enumerate().skip(s0) {
            let t = (i - s0) as f32 / sr as f32;
            let env = (-t * 6.0).exp() * (t * 400.0).min(1.0); // quick attack, soft decay
            let tone = (std::f32::consts::TAU * freq * t).sin() + 0.25 * (std::f32::consts::TAU * freq * 2.0 * t).sin();
            *o += 0.16 * env * tone;
        }
    }
    (out, sr)
}

// ---------- output devices ----------

pub fn output_device_names() -> Vec<String> {
    let host = cpal::default_host();
    host.output_devices()
        .map(|it| it.filter_map(|d| d.description().ok().map(|x| x.name().to_string())).collect())
        .unwrap_or_default()
}

/// Picks the virtual cable automatically if the user hasn't chosen one.
pub fn guess_virtual_device(names: &[String]) -> Option<String> {
    const HINTS: [&str; 6] = ["cable input", "vb-audio", "blackhole", "corvox", "virtual", "loopback"];
    HINTS.iter().find_map(|h| names.iter().find(|n| n.to_lowercase().contains(h)).cloned())
}

fn find_device(name: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    host.output_devices().ok()?.find(|d| d.description().map(|x| x.name() == name).unwrap_or(false))
}

fn default_device_name() -> Option<String> {
    cpal::default_host().default_output_device()?.description().ok().map(|d| d.name().to_string())
}

/// One running output stream fed from a queue of mono samples at the device rate.
struct Sink {
    _stream: cpal::Stream,
    queue: Arc<Mutex<VecDeque<f32>>>,
    rate: u32,
    gain: Arc<Mutex<f32>>,
}

impl Sink {
    fn open(device: &cpal::Device, gain: f32) -> Result<Self> {
        let cfg = device.default_output_config()?;
        let rate = cfg.sample_rate();
        let channels = cfg.channels() as usize;
        let queue = Arc::new(Mutex::new(VecDeque::<f32>::new()));
        let gain = Arc::new(Mutex::new(gain));
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(device, cfg.into(), channels, queue.clone(), gain.clone())?,
            cpal::SampleFormat::I16 => build::<i16>(device, cfg.into(), channels, queue.clone(), gain.clone())?,
            cpal::SampleFormat::U16 => build::<u16>(device, cfg.into(), channels, queue.clone(), gain.clone())?,
            cpal::SampleFormat::I32 => build::<i32>(device, cfg.into(), channels, queue.clone(), gain.clone())?,
            f => return Err(anyhow!("unsupported sample format {f}")),
        };
        stream.play()?;
        Ok(Self { _stream: stream, queue, rate, gain })
    }
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    queue: Arc<Mutex<VecDeque<f32>>>,
    gain: Arc<Mutex<f32>>,
) -> Result<cpal::Stream> {
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let g = *gain.lock().unwrap();
            let mut q = queue.lock().unwrap();
            for frame in data.chunks_mut(channels) {
                let v = q.pop_front().unwrap_or(0.0) * g;
                for s in frame.iter_mut() {
                    *s = T::from_sample(v);
                }
            }
        },
        |e| eprintln!("audio stream error: {e}"),
        None,
    )?;
    Ok(stream)
}

/// Plays speech to the "mic" device (the virtual cable) and optionally to your speakers.
pub struct Player {
    mic: Option<Sink>,
    monitor: Option<Sink>,
    /// Speakers for app sounds (the "ready" chime) when "hear yourself" is off.
    local: Option<Sink>,
    mic_name: Option<String>,
    pub error: Option<String>,
}

impl Player {
    pub fn new(mic_device: Option<&str>, monitor: bool, monitor_gain: f32) -> Self {
        let mut error = None;
        let mic = match mic_device {
            Some(name) => match find_device(name).ok_or_else(|| anyhow!("device \"{name}\" not found")).and_then(|d| Sink::open(&d, 1.0)) {
                Ok(s) => Some(s),
                Err(e) => {
                    error = Some(format!("Mic output: {e}"));
                    None
                }
            },
            None => None,
        };
        let default = default_device_name();
        let monitor = if monitor && default.as_deref() != mic_device {
            cpal::default_host().default_output_device().and_then(|d| Sink::open(&d, monitor_gain).ok())
        } else {
            None
        };
        Self { mic, monitor, local: None, mic_name: mic_device.map(String::from), error }
    }

    pub fn has_mic(&self) -> bool {
        self.mic.is_some()
    }

    pub fn set_monitor_gain(&self, g: f32) {
        if let Some(m) = &self.monitor {
            *m.gain.lock().unwrap() = g;
        }
    }

    fn sinks(&self) -> impl Iterator<Item = &Sink> {
        self.mic.iter().chain(self.monitor.iter())
    }

    pub fn push(&self, samples: &[f32], sr: u32) {
        for s in self.sinks() {
            let r = resample(samples, sr, s.rate);
            s.queue.lock().unwrap().extend(r);
        }
    }

    pub fn push_silence(&self, seconds: f32) {
        for s in self.sinks() {
            let n = (seconds * s.rate as f32) as usize;
            s.queue.lock().unwrap().extend(std::iter::repeat_n(0.0, n));
        }
    }

    /// Plays a sound only on your speakers/headphones, never into the virtual mic.
    pub fn play_local(&mut self, samples: &[f32], sr: u32) {
        if self.monitor.is_none() && self.local.is_none() && default_device_name() != self.mic_name {
            self.local = cpal::default_host().default_output_device().and_then(|d| Sink::open(&d, 0.8).ok());
        }
        if let Some(s) = self.monitor.as_ref().or(self.local.as_ref()) {
            s.queue.lock().unwrap().extend(resample(samples, sr, s.rate));
        }
    }

    pub fn is_playing(&self) -> bool {
        self.sinks().any(|s| !s.queue.lock().unwrap().is_empty())
    }

    pub fn stop(&self) {
        for s in self.sinks() {
            s.queue.lock().unwrap().clear();
        }
    }

    fn local_sink(&self) -> Option<&Sink> {
        self.monitor.as_ref().or(self.local.as_ref())
    }

    /// Whether something from [`Player::play_local`] is still playing.
    pub fn local_playing(&self) -> bool {
        self.local_sink().is_some_and(|s| !s.queue.lock().unwrap().is_empty())
    }

    pub fn stop_local(&self) {
        if let Some(s) = self.local_sink() {
            s.queue.lock().unwrap().clear();
        }
    }
}

// ---------- recording from a microphone ----------

/// Input devices that are really the virtual cable (recording those captures Corvox itself).
fn looks_virtual(name: &str) -> bool {
    const HINTS: [&str; 6] = ["cable output", "vb-audio", "blackhole", "corvox", "monitor of", "loopback"];
    let n = name.to_lowercase();
    HINTS.iter().any(|h| n.contains(h))
}

pub fn input_device_names() -> Vec<String> {
    cpal::default_host()
        .input_devices()
        .map(|it| it.filter_map(|d| d.description().ok().map(|x| x.name().to_string())).collect())
        .unwrap_or_default()
}

/// The microphone used when none is chosen: the system's default recording device, unless
/// that's the virtual cable, then the first real one.
pub fn default_input_name() -> Option<String> {
    let default = cpal::default_host().default_input_device().and_then(|d| d.description().ok()).map(|d| d.name().to_string());
    match default {
        Some(d) if !looks_virtual(&d) => Some(d),
        other => input_device_names().into_iter().find(|n| !looks_virtual(n)).or(other),
    }
}

/// Recordings stop growing here, even if nobody presses Stop.
const MAX_RECORD_SECONDS: f32 = 60.0;

/// A running microphone recording: mono samples at the device's rate, plus a live level.
pub struct Recorder {
    _stream: cpal::Stream,
    samples: Arc<Mutex<Vec<f32>>>,
    /// Loudest sample since the UI last looked (f32 bits).
    peak: Arc<AtomicU32>,
    pub rate: u32,
}

impl Recorder {
    /// Starts recording from `device` (by name), or the default microphone.
    pub fn start(device: Option<&str>) -> Result<Self> {
        let host = cpal::default_host();
        let name = device.map(String::from).or_else(default_input_name).ok_or_else(|| anyhow!("no microphone found"))?;
        let dev = host
            .input_devices()?
            .find(|d| d.description().map(|x| x.name() == name).unwrap_or(false))
            .ok_or_else(|| anyhow!("microphone \"{name}\" not found"))?;
        let cfg = dev.default_input_config()?;
        let rate = cfg.sample_rate();
        let channels = cfg.channels() as usize;
        let samples = Arc::new(Mutex::new(Vec::with_capacity(rate as usize * 30)));
        let peak = Arc::new(AtomicU32::new(0));
        let max = (MAX_RECORD_SECONDS * rate as f32) as usize;
        let (s, p) = (samples.clone(), peak.clone());
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => build_input::<f32>(&dev, cfg.into(), channels, max, s, p)?,
            cpal::SampleFormat::I16 => build_input::<i16>(&dev, cfg.into(), channels, max, s, p)?,
            cpal::SampleFormat::U16 => build_input::<u16>(&dev, cfg.into(), channels, max, s, p)?,
            cpal::SampleFormat::I32 => build_input::<i32>(&dev, cfg.into(), channels, max, s, p)?,
            cpal::SampleFormat::U8 => build_input::<u8>(&dev, cfg.into(), channels, max, s, p)?,
            f => return Err(anyhow!("unsupported microphone format {f}")),
        };
        stream.play()?;
        Ok(Self { _stream: stream, samples, peak, rate })
    }

    /// The loudest sample since the last call (0–1).
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }

    pub fn seconds(&self) -> f32 {
        self.samples.lock().unwrap().len() as f32 / self.rate as f32
    }

    /// Stops recording and hands over what was captured.
    pub fn finish(self) -> (Vec<f32>, u32) {
        let rate = self.rate;
        let samples = std::mem::take(&mut *self.samples.lock().unwrap());
        drop(self);
        (samples, rate)
    }
}

fn build_input<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    max: usize,
    samples: Arc<Mutex<Vec<f32>>>,
    peak: Arc<AtomicU32>,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = channels.max(1);
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            let mut buf = samples.lock().unwrap();
            let mut loudest = 0.0f32;
            for frame in data.chunks(channels) {
                let v = frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / channels as f32;
                loudest = loudest.max(v.abs());
                if buf.len() < max {
                    buf.push(v);
                }
            }
            // For non-negative floats the bit patterns sort like the numbers.
            peak.fetch_max(loudest.to_bits(), Ordering::Relaxed);
        },
        |e| eprintln!("microphone stream error: {e}"),
        None,
    )?;
    Ok(stream)
}

/// How a finished recording sounds, for a hint before saving it.
#[derive(Debug, PartialEq)]
pub enum TakeIssue {
    Silent,
    TooQuiet,
    Clipped,
    TooShort,
}

pub fn check_take(samples: &[f32], sr: u32) -> Option<TakeIssue> {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak < 1e-4 {
        return Some(TakeIssue::Silent);
    }
    let speech = tidy_clip(samples, sr).len() as f32 / sr as f32;
    if speech < 3.0 {
        return Some(TakeIssue::TooShort);
    }
    if peak < 0.03 {
        return Some(TakeIssue::TooQuiet);
    }
    let clipped = samples.iter().filter(|s| s.abs() > 0.99).count();
    if clipped as f32 > sr as f32 * 0.005 {
        return Some(TakeIssue::Clipped);
    }
    None
}

/// Saves a recording as a voice reference, cleaned up like an imported file. Returns its
/// length in seconds.
pub fn save_take(samples: &[f32], sr: u32, dest: &Path) -> Result<f32> {
    let clip = tidy_clip(&resample(samples, sr, VOICE_SR), VOICE_SR);
    let secs = clip.len() as f32 / VOICE_SR as f32;
    if secs < 2.0 {
        return Err(anyhow!("the recording has less than 2 seconds of speech"));
    }
    write_wav(dest, &clip, VOICE_SR)?;
    Ok(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_stereo_44k_wav_becomes_clean_24k_mono() {
        let dir = std::env::temp_dir().join(format!("corvox-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("in.wav");
        let spec = hound::WavSpec { channels: 2, sample_rate: 44_100, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&src, spec).unwrap();
        // 1 s silence, 5 s of a 220 Hz tone, 1 s silence
        for i in 0..(44_100 * 7) {
            let t = i as f32 / 44_100.0;
            let v = if (1.0..6.0).contains(&t) { (t * 220.0 * std::f32::consts::TAU).sin() * 0.3 } else { 0.0 };
            w.write_sample((v * 32767.0) as i16).unwrap();
            w.write_sample((v * 32767.0) as i16).unwrap();
        }
        w.finalize().unwrap();

        let dest = dir.join("out.wav");
        let secs = import_voice(&src, &dest).unwrap();
        assert!((5.0..5.4).contains(&secs), "silence should be trimmed, got {secs}");
        let r = hound::WavReader::open(&dest).unwrap();
        assert_eq!(r.spec().sample_rate, VOICE_SR);
        assert_eq!(r.spec().channels, 1);
        let peak = r.into_samples::<i16>().flatten().map(|s| (s as i32).abs()).max().unwrap();
        assert!(peak > 28_000, "peak should be normalized to about -1 dBFS, got {peak}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn checks_a_recording_before_saving() {
        let sr = 48_000;
        let tone = |secs: f32, amp: f32| -> Vec<f32> { (0..(secs * sr as f32) as usize).map(|i| (i as f32 * 0.05).sin() * amp).collect() };
        assert_eq!(check_take(&vec![0.0; sr as usize * 5], sr), Some(TakeIssue::Silent));
        assert_eq!(check_take(&tone(1.5, 0.5), sr), Some(TakeIssue::TooShort));
        assert_eq!(check_take(&tone(6.0, 0.01), sr), Some(TakeIssue::TooQuiet));
        assert_eq!(check_take(&tone(6.0, 1.5).iter().map(|s| s.clamp(-1.0, 1.0)).collect::<Vec<_>>(), sr), Some(TakeIssue::Clipped));
        assert_eq!(check_take(&tone(6.0, 0.5), sr), None);

        let dest = std::env::temp_dir().join(format!("corvox-take-{}.wav", std::process::id()));
        let secs = save_take(&tone(6.0, 0.5), sr, &dest).unwrap();
        assert!((5.8..6.1).contains(&secs), "got {secs}");
        assert_eq!(hound::WavReader::open(&dest).unwrap().spec().sample_rate, VOICE_SR);
        let _ = std::fs::remove_file(dest);
    }

    #[test]
    fn resample_keeps_length_ratio() {
        let x: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.01).sin()).collect();
        assert_eq!(resample(&x, 48_000, 24_000).len(), 24_000);
        assert_eq!(resample(&x, 48_000, 44_100).len(), 44_100);
    }
}
