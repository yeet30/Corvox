from __future__ import annotations

import threading
import time

import psutil


def vram_mb() -> float | None:
    try:
        import torch

        if torch.cuda.is_available() and torch.cuda.is_initialized():
            return round(torch.cuda.max_memory_reserved() / 2**20, 1)
    except Exception:
        pass
    return None


def mem_stats() -> dict:
    rss = psutil.Process().memory_info().rss
    return {"ram_mb": round(rss / 2**20, 1), "vram_mb": vram_mb()}


class PeakRss:
    """Samples this process's resident memory in the background and keeps the peak."""

    def __init__(self, interval: float = 0.05) -> None:
        self.interval = interval
        self.peak = 0
        self._stop = threading.Event()
        self._proc = psutil.Process()

    def __enter__(self):
        self.peak = self._proc.memory_info().rss
        self._t = threading.Thread(target=self._run, daemon=True)
        self._t.start()
        return self

    def _run(self):
        while not self._stop.is_set():
            self.peak = max(self.peak, self._proc.memory_info().rss)
            time.sleep(self.interval)

    def __exit__(self, *exc):
        self._stop.set()
        self._t.join()
        self.peak = max(self.peak, self._proc.memory_info().rss)

    @property
    def peak_mb(self) -> float:
        return round(self.peak / 2**20, 1)
