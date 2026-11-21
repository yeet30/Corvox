"""Cleans the edges of generated speech while it streams.

Cloning models sometimes add a short whimper, breath or whistle before the first word or
after the last one. EdgeTrimmer:

- start: holds audio until speech actually begins (or 0.6 s pass), drops what came before
  (keeping 120 ms, so soft consonants like a starting "t" survive), and fades in over 10 ms;
- end: always holds back the last 0.35 s; when the stream finishes, cuts anything after the
  last speech (keeping 200 ms) and fades out over 25 ms.

"Speech" means a 20 ms frame louder than an absolute floor and a fraction of the loudest
frame so far, so quiet noise around the words is removed but soft words are kept.
"""

from __future__ import annotations

import numpy as np

FRAME_S = 0.02
LEAD_MAX_S = 0.6
LEAD_KEEP_S = 0.12
TAIL_HOLD_S = 0.35
TAIL_KEEP_S = 0.2
FADE_IN_S = 0.01
FADE_OUT_S = 0.025
FLOOR = 0.008
RELATIVE = 0.06


def _frame_rms(a: np.ndarray, frame: int) -> np.ndarray:
    n = len(a) // frame
    if n == 0:
        return np.zeros(0, dtype=np.float32)
    x = a[: n * frame].reshape(n, frame).astype(np.float64)
    return np.sqrt((x * x).mean(axis=1)).astype(np.float32)


class EdgeTrimmer:
    def __init__(self, sr: int) -> None:
        self.sr = sr
        self.frame = max(1, int(sr * FRAME_S))
        self.started = False
        self.lead = np.zeros(0, dtype=np.float32)
        self.tail = np.zeros(0, dtype=np.float32)
        self.peak = 0.0

    def _threshold(self) -> float:
        return max(FLOOR, RELATIVE * self.peak)

    def _note_peak(self, a: np.ndarray) -> None:
        r = _frame_rms(a, self.frame)
        if len(r):
            self.peak = max(self.peak, float(r.max()))

    def push(self, audio: np.ndarray) -> np.ndarray:
        """Feed a chunk; returns the audio that's safe to play now (may be empty)."""
        audio = np.asarray(audio, dtype=np.float32).reshape(-1)
        if not self.started:
            self.lead = np.concatenate([self.lead, audio])
            self._note_peak(self.lead)
            r = _frame_rms(self.lead, self.frame)
            loud = np.nonzero(r > max(FLOOR, RELATIVE * self.peak))[0]
            if len(loud) == 0 and len(self.lead) < LEAD_MAX_S * self.sr:
                return np.zeros(0, dtype=np.float32)
            start = 0
            if len(loud):
                start = max(0, int(loud[0]) * self.frame - int(LEAD_KEEP_S * self.sr))
            audio = self.lead[start:].copy()
            fade = min(len(audio), int(FADE_IN_S * self.sr))
            if fade:
                audio[:fade] *= np.linspace(0.0, 1.0, fade, dtype=np.float32)
            self.lead = np.zeros(0, dtype=np.float32)
            self.started = True
        else:
            self._note_peak(audio)
        self.tail = np.concatenate([self.tail, audio])
        hold = int(TAIL_HOLD_S * self.sr)
        if len(self.tail) <= hold:
            return np.zeros(0, dtype=np.float32)
        out, self.tail = self.tail[:-hold], self.tail[-hold:]
        return out

    def finish(self) -> np.ndarray:
        """End of stream: returns the held-back audio with trailing noise removed."""
        rest = self.tail if self.started else self.lead
        self.tail = self.lead = np.zeros(0, dtype=np.float32)
        if len(rest) == 0:
            return rest
        r = _frame_rms(rest, self.frame)
        loud = np.nonzero(r > self._threshold())[0]
        if len(loud):
            end = min(len(rest), (int(loud[-1]) + 1) * self.frame + int(TAIL_KEEP_S * self.sr))
        else:
            end = min(len(rest), int(TAIL_KEEP_S * self.sr))
        rest = rest[:end].copy()
        fade = min(len(rest), int(FADE_OUT_S * self.sr))
        if fade:
            rest[-fade:] *= np.linspace(1.0, 0.0, fade, dtype=np.float32)
        return rest
