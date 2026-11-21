from __future__ import annotations

import os
import wave

import numpy as np


class Adapter:
    """One TTS model. Subclasses load lazily and synthesize one chunk of text at a time."""

    id: str = ""
    gpu_capable: bool = False

    def __init__(self) -> None:
        self.device = "cpu"
        self.loaded = False

    def pick_device(self) -> str:
        if not self.gpu_capable or os.environ.get("CORVOX_FORCE_CPU") == "1":
            return "cpu"
        try:
            import torch

            return "cuda" if torch.cuda.is_available() else "cpu"
        except Exception:
            return "cpu"

    def load(self, language: str | None = None) -> None:
        raise NotImplementedError

    def synth(self, text: str, voice_path: str, language: str) -> tuple[np.ndarray, int]:
        """Return (float32 mono samples in [-1, 1], sample rate). `voice_path` is already prepared."""
        raise NotImplementedError

    def warm(self, voice_path: str, language: str) -> None:
        """Optional: precompute whatever the model needs from the voice clip."""

    def synth_stream(self, text: str, voice_path: str, language: str, stop):
        """Yield (samples, sample_rate) pieces as they're ready. Default: one sentence at a time,
        each with its own edges cleaned (noises can appear between sentences too)."""
        from ..postprocess import EdgeTrimmer

        for sentence in split_sentences(text):
            if stop.is_set():
                return
            audio, sr = self.synth(sentence, voice_path, language)
            t = EdgeTrimmer(sr)
            cleaned = np.concatenate([t.push(audio), t.finish()])
            yield cleaned, sr


def read_wav_mono(path: str) -> tuple[np.ndarray, int]:
    """Read a PCM WAV (the app writes 16-bit mono 24 kHz) into float32 mono."""
    with wave.open(path, "rb") as w:
        sr = w.getframerate()
        ch = w.getnchannels()
        width = w.getsampwidth()
        data = w.readframes(w.getnframes())
    if width == 2:
        a = np.frombuffer(data, dtype="<i2").astype(np.float32) / 32768.0
    elif width == 4:
        a = np.frombuffer(data, dtype="<i4").astype(np.float32) / 2147483648.0
    elif width == 1:
        a = (np.frombuffer(data, dtype=np.uint8).astype(np.float32) - 128.0) / 128.0
    else:
        raise ValueError(f"unsupported WAV sample width {width}")
    if ch > 1:
        a = a.reshape(-1, ch).mean(axis=1)
    return a, sr


def to_numpy(audio) -> np.ndarray:
    try:
        import torch

        if isinstance(audio, torch.Tensor):
            audio = audio.detach().float().cpu().numpy()
    except ImportError:
        pass
    return np.asarray(audio, dtype=np.float32).reshape(-1)


# ---------- reference clip preparation ----------
#
# Cloning models continue from the end of the reference clip. A clip that stops mid-word
# makes them babble or invent words before reading your text (measured: clarity 14% with a
# 20 s clip that ended mid-word, 95% with the same clip cut at a pause around 7-10 s).

def _pauses(a: np.ndarray, sr: int, min_ms: int = 200) -> list[float]:
    win = sr // 50
    if len(a) < win * 4:
        return []
    r = np.sqrt(np.convolve(a.astype(np.float64) ** 2, np.ones(win) / win, mode="valid")[::win])
    thr = np.median(r) * 0.15
    quiet = r < thr
    res, i, need = [], 0, max(1, min_ms // 20)
    while i < len(quiet):
        if quiet[i]:
            j = i
            while j < len(quiet) and quiet[j]:
                j += 1
            if j - i >= need:
                res.append((i + j) / 2 * win / sr)
            i = j
        else:
            i += 1
    return res


def prepare_reference(path: str, max_seconds: float = 10.0) -> str:
    """Returns a cached copy of the clip that ends at a natural pause (<= max_seconds),
    followed by half a second of silence."""
    import hashlib
    from pathlib import Path

    st = os.stat(path)
    key = hashlib.sha1(f"{path}|{st.st_mtime_ns}|{st.st_size}|{max_seconds}".encode()).hexdigest()[:16]
    out = Path(os.environ.get("CORVOX_CACHE", Path.home() / ".cache" / "corvox")) / "refs" / f"{key}.wav"
    if out.exists():
        return str(out)
    a, sr = read_wav_mono(path)
    pauses = [p for p in _pauses(a, sr) if 4.0 <= p <= max_seconds]
    if pauses:
        end = pauses[-1]
    elif len(a) / sr <= max_seconds:
        end = len(a) / sr
    else:
        # no clean pause: cut at the quietest 20 ms between 6 s and max_seconds
        win = sr // 50
        lo, hi = int(6 * sr), int(max_seconds * sr)
        seg = a[lo:hi]
        e = [float(np.mean(seg[i:i + win] ** 2)) for i in range(0, max(1, len(seg) - win), win)]
        end = (lo + int(np.argmin(e)) * win) / sr
    clip = np.concatenate([a[: int(end * sr)], np.zeros(sr // 2, dtype=np.float32)])
    out.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(out), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(sr)
        w.writeframes((np.clip(clip, -1, 1) * 32767).astype("<i2").tobytes())
    return str(out)


def split_sentences(text: str, max_len: int = 220) -> list[str]:
    import re

    parts = re.split(r"(?<=[.!?…])\s+|\n+", text.strip())
    out: list[str] = []
    for p in parts:
        p = p.strip()
        while len(p) > max_len:
            cut = p.rfind(" ", 0, max_len)
            cut = cut if cut > 40 else max_len
            out.append(p[:cut].strip())
            p = p[cut:].strip()
        if p:
            out.append(p)
    return out
