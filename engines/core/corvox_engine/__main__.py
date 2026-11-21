"""Entry point: `python -m corvox_engine` (serve) or `python -m corvox_engine --bench ...`."""

from __future__ import annotations

# Must be first: moves the real stdout aside before any library can print to it.
from . import protocol  # noqa: F401  isort:skip

import argparse
import queue
import threading
import time
import traceback

import numpy as np

from .adapters.base import prepare_reference
from .downloads import DownloadWatch
from .postprocess import EdgeTrimmer
from .protocol import log, pcm16_b64, read_requests, send
from .registry import make_adapter
from .stats import mem_stats

CHUNK_SECONDS = 0.25  # group streamed frames into ~250 ms messages


class Server:
    def __init__(self) -> None:
        self.adapters = {}
        self.requests: queue.Queue = queue.Queue()
        self.speaking_id = None
        self.stop = threading.Event()

    def get(self, model: str, req_id, language: str | None = None, model_mb: float | None = None):
        ad = self.adapters.get(model)
        if ad is None:
            ad = make_adapter(model)
            self.adapters[model] = ad
        if not ad.loaded:
            send({"event": "status", "id": req_id, "msg": "Loading the model…"})
            t = time.perf_counter()
            with DownloadWatch(req_id, "model", model_mb):
                ad.load(language)
            log(f"loaded {model} on {ad.device} in {time.perf_counter() - t:.1f}s")
        return ad

    def speak(self, req: dict, rid) -> None:
        ad = self.get(req["model"], rid, req.get("language"), req.get("model_mb"))
        voice = prepare_reference(req["voice"])
        self.stop.clear()
        self.speaking_id = rid
        total, buf, buf_sr = 0.0, [], 24000
        trimmer = None

        def flush(force: bool = False) -> None:
            nonlocal buf, total
            n = sum(len(b) for b in buf)
            if n and (force or n / buf_sr >= CHUNK_SECONDS):
                out = np.concatenate(buf)
                buf = []
                total += len(out) / buf_sr
                send({"event": "chunk", "id": rid, "sample_rate": buf_sr, "pcm16": pcm16_b64(out)})

        try:
            for audio, sr in ad.synth_stream(req["text"], voice, req.get("language", "en"), self.stop):
                if self.stop.is_set():
                    break
                if trimmer is None:
                    trimmer, buf_sr = EdgeTrimmer(sr), sr
                ready = trimmer.push(audio)
                if len(ready):
                    buf.append(ready)
                    flush()
            if trimmer is not None and not self.stop.is_set():
                buf.append(trimmer.finish())
                flush(force=True)
        finally:
            self.speaking_id = None
        send({"id": rid, "ok": True, "seconds": round(total, 2), "cancelled": self.stop.is_set()})

    def handle(self, req: dict) -> None:
        rid = req.get("id")
        cmd = req.get("cmd")
        try:
            if cmd == "hello":
                send({"id": rid, "ok": True, **mem_stats()})
            elif cmd == "load":
                t = time.perf_counter()
                ad = self.get(req["model"], rid, req.get("language"), req.get("model_mb"))
                if req.get("voice"):
                    ad.warm(prepare_reference(req["voice"]), req.get("language", "en"))
                send({"id": rid, "ok": True, "device": ad.device,
                      "load_seconds": round(time.perf_counter() - t, 2), **mem_stats()})
            elif cmd == "speak":
                self.speak(req, rid)
            elif cmd == "unload":
                self.adapters.pop(req.get("model"), None)
                import gc

                gc.collect()
                send({"id": rid, "ok": True})
            elif cmd == "quit":
                send({"id": rid, "ok": True})
                raise SystemExit(0)
            else:
                send({"id": rid, "ok": False, "error": f"unknown command {cmd!r}"})
        except SystemExit:
            raise
        except Exception as e:
            traceback.print_exc()
            send({"id": rid, "ok": False, "error": f"{type(e).__name__}: {e}"})

    def _reader(self) -> None:
        # Runs beside the main loop so "cancel" can interrupt a speak that's in progress.
        for req in read_requests():
            if req.get("cmd") == "cancel":
                if self.speaking_id is not None:
                    self.stop.set()
                # drop queued speaks too
                kept = []
                while not self.requests.empty():
                    r = self.requests.get_nowait()
                    if r.get("cmd") != "speak":
                        kept.append(r)
                    else:
                        send({"id": r.get("id"), "ok": True, "seconds": 0, "cancelled": True})
                for r in kept:
                    self.requests.put(r)
                continue
            self.requests.put(req)
        self.requests.put({"cmd": "quit", "id": 0})

    def run(self) -> None:
        threading.Thread(target=self._reader, daemon=True).start()
        # Importing PyTorch takes ~5 s on Windows; start now so it overlaps with the app
        # sending its first request (a later import just waits for this one to finish).
        threading.Thread(target=lambda: __import__("torch"), daemon=True).start()
        send({"event": "ready"})
        while True:
            self.handle(self.requests.get())


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--bench", action="store_true")
    ap.add_argument("--model")
    ap.add_argument("--voice")
    ap.add_argument("--language", default="en")
    args = ap.parse_args()
    if args.bench:
        from .bench import run_bench

        try:
            result = run_bench(args.model, prepare_reference(args.voice), args.language)
            send({"event": "bench_result", "ok": True, **result})
        except Exception as e:
            traceback.print_exc()
            send({"event": "bench_result", "ok": False, "error": f"{type(e).__name__}: {e}"})
        return
    Server().run()


if __name__ == "__main__":
    main()
