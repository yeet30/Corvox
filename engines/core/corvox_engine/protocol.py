"""Line-based JSON protocol between the Rust app and this helper.

The app writes one JSON object per line to our stdin and reads one JSON object
per line from our stdout. Model libraries love printing to stdout, so before
anything else we move the real stdout to a private file descriptor and point
fd 1 (and sys.stdout) at stderr. Only `send()` writes to the real stdout.
"""

from __future__ import annotations

import base64
import json
import os
import sys
import threading

import numpy as np

_proto_fd = os.dup(1)
os.dup2(2, 1)
sys.stdout = sys.stderr
_out = os.fdopen(_proto_fd, "w", encoding="utf-8", newline="\n", buffering=1)
_lock = threading.Lock()

# Same trick for stdin: read requests from a private (non-inheritable) copy and give fd 0
# to /dev/null. On Windows, a thread blocked reading an inheritable pipe makes any child
# process creation (libraries spawn some during model load) hang forever.
_in_fd = os.dup(0)
_null = os.open(os.devnull, os.O_RDONLY)
os.dup2(_null, 0)
os.close(_null)
_in = os.fdopen(_in_fd, "rb")
sys.stdin = open(os.devnull, "r")


def send(obj: dict) -> None:
    line = json.dumps(obj, ensure_ascii=False)
    with _lock:
        _out.write(line + "\n")
        _out.flush()


def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)
    send({"event": "log", "msg": msg})


def read_requests():
    """Yield parsed requests from stdin until it closes."""
    for raw in iter(_in.readline, b""):
        line = raw.decode("utf-8", errors="replace").strip()
        if not line:
            continue
        try:
            yield json.loads(line)
        except json.JSONDecodeError as e:
            send({"event": "log", "msg": f"bad request line: {e}"})


def pcm16_b64(audio: np.ndarray) -> str:
    audio = np.asarray(audio, dtype=np.float32).reshape(-1)
    audio = np.clip(audio, -1.0, 1.0)
    return base64.b64encode((audio * 32767.0).astype("<i2").tobytes()).decode("ascii")
