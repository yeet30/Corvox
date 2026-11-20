"""Reports model-download progress by watching the folders model libraries download into.

Hugging Face, Coqui and Whisper don't share a progress API, but they all write files to
known cache folders, so we sum the bytes of files that appeared or grew since loading began.
"""

from __future__ import annotations

import os
import sys
import threading
import time
from pathlib import Path

from .protocol import send


def watched_dirs() -> list[Path]:
    home = Path.home()
    hf = Path(os.environ.get("HF_HOME", home / ".cache" / "huggingface"))
    dirs = [Path(os.environ.get("HF_HUB_CACHE", hf / "hub")), home / ".cache" / "corvox"]
    if sys.platform == "win32":
        dirs.append(Path(os.environ.get("LOCALAPPDATA", home / "AppData" / "Local")) / "tts")
    elif sys.platform == "darwin":
        dirs.append(home / "Library" / "Application Support" / "tts")
    else:
        dirs.append(Path(os.environ.get("XDG_DATA_HOME", home / ".local" / "share")) / "tts")
    return dirs


def _new_bytes(dirs: list[Path], since: float, baseline: dict[str, int]) -> int:
    total = 0
    for d in dirs:
        if not d.exists():
            continue
        for root, _, files in os.walk(d):
            for f in files:
                p = os.path.join(root, f)
                try:
                    st = os.stat(p)
                except OSError:
                    continue
                if f.endswith(".incomplete"):
                    # partial downloads count fully, so a resumed download shows its real progress
                    total += st.st_size
                elif st.st_mtime >= since:
                    total += max(0, st.st_size - baseline.get(p, 0))
    return total


class DownloadWatch:
    """Context manager: while active, sends {"event": "progress", ...} every half second
    once something is actually downloading."""

    def __init__(self, req_id, what: str, total_mb: float | None) -> None:
        self.req_id, self.what, self.total_mb = req_id, what, total_mb
        self._stop = threading.Event()

    def __enter__(self):
        self.dirs = watched_dirs()
        self.since = time.time() - 1
        # sizes of partial files that already exist (e.g. an interrupted earlier download)
        self.baseline = {}
        for d in self.dirs:
            if d.exists():
                for root, _, files in os.walk(d):
                    for f in files:
                        p = os.path.join(root, f)
                        try:
                            self.baseline[p] = os.stat(p).st_size
                        except OSError:
                            pass
        self._t = threading.Thread(target=self._run, daemon=True)
        self._t.start()
        return self

    def _run(self):
        last = -1
        while not self._stop.wait(0.5):
            done = _new_bytes(self.dirs, self.since, self.baseline) / 2**20
            if done > 1 and int(done) != last:
                last = int(done)
                send({"event": "progress", "id": self.req_id, "what": self.what,
                      "done_mb": round(done, 1), "total_mb": self.total_mb})

    def __exit__(self, *exc):
        self._stop.set()
        self._t.join()
