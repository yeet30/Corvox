"""'Test on my PC': measures one engine with the user's own voice clip.

Runs in a fresh helper process so RAM/VRAM numbers aren't polluted by other models.

- speed:       seconds of audio produced per second of compute (2.0 = twice real time)
- ram_mb:      peak resident memory of this process while speaking (model loaded)
- vram_mb:     peak GPU memory reserved by PyTorch (GPU engines only)
- voice_match: cosine similarity (0-100) between speaker embeddings of the clip and the output
- clarity:     100 * (1 - character error rate) of a Whisper transcript of the output
"""

from __future__ import annotations

import os
import re
import time
import unicodedata
import urllib.request
from pathlib import Path

import numpy as np

from .adapters.base import read_wav_mono
from .protocol import send
from .registry import make_adapter
from .stats import PeakRss, vram_mb

SPEAKER_MODEL_URL = (
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/"
    "speaker-recongition-models/wespeaker_en_voxceleb_resnet34.onnx"
)
WHISPER_SIZE = os.environ.get("CORVOX_WHISPER", "base")

TEST_TEXT = {
    "tr": ["Bugün hava çok güzel, akşam birlikte oyun oynayalım mı?",
           "Yarın sabah erkenden kalkıp markete gitmem gerekiyor."],
    "en": ["The weather is lovely today, do you want to play a few rounds tonight?",
           "I need to get up early tomorrow and go to the store."],
    "fr": ["Il fait très beau aujourd'hui, on joue ensemble ce soir ?",
           "Demain matin, je dois me lever tôt pour aller au marché."],
    "de": ["Heute ist das Wetter wirklich schön, spielen wir heute Abend zusammen?",
           "Morgen früh muss ich zeitig aufstehen und einkaufen gehen."],
    "es": ["Hoy hace un tiempo precioso, ¿jugamos juntos esta noche?",
           "Mañana tengo que levantarme temprano para ir al mercado."],
    "it": ["Oggi il tempo è bellissimo, giochiamo insieme stasera?",
           "Domani mattina devo alzarmi presto per andare al mercato."],
    "pt": ["O tempo está ótimo hoje, vamos jogar juntos esta noite?",
           "Amanhã de manhã preciso acordar cedo para ir ao mercado."],
    "nl": ["Het is vandaag prachtig weer, zullen we vanavond samen spelen?",
           "Morgenochtend moet ik vroeg opstaan om naar de markt te gaan."],
}


def progress(msg: str) -> None:
    send({"event": "status", "msg": msg})


def cache_dir() -> Path:
    d = Path(os.environ.get("CORVOX_CACHE", Path.home() / ".cache" / "corvox"))
    d.mkdir(parents=True, exist_ok=True)
    return d


def resample(audio: np.ndarray, sr: int, target: int) -> np.ndarray:
    if sr == target:
        return audio.astype(np.float32)
    from math import gcd

    from scipy.signal import resample_poly

    g = gcd(sr, target)
    return resample_poly(audio, target // g, sr // g).astype(np.float32)


def speaker_embedding(extractor, audio: np.ndarray, sr: int) -> np.ndarray:
    stream = extractor.create_stream()
    stream.accept_waveform(sample_rate=16000, waveform=resample(audio, sr, 16000))
    stream.input_finished()
    e = np.array(extractor.compute(stream), dtype=np.float32)
    return e / (np.linalg.norm(e) + 1e-9)


def normalize(s: str) -> str:
    s = unicodedata.normalize("NFKC", s).casefold()
    s = re.sub(r"[^\w\s]", " ", s)
    return re.sub(r"\s+", " ", s).strip()


def run_bench(model: str, voice: str, language: str) -> dict:
    texts = TEST_TEXT.get(language, TEST_TEXT["en"])
    ad = make_adapter(model)

    progress("Loading the model…")
    t = time.perf_counter()
    ad.load(language)
    load_s = time.perf_counter() - t
    import gc

    gc.collect()

    # RAM is measured while speaking (after loading), which is what you live with day to day.
    with PeakRss() as peak:
        progress("Warming up…")
        ad.synth(texts[0], voice, language)  # full sentence: int8 kernels need a real warm-up

        outputs = []
        gen_s = 0.0
        audio_s = 0.0
        for i, text in enumerate(texts, 1):
            progress(f"Speaking test sentence {i}/{len(texts)}…")
            t = time.perf_counter()
            audio, sr = ad.synth(text, voice, language)
            gen_s += time.perf_counter() - t
            audio_s += len(audio) / sr
            outputs.append((text, audio, sr))
    ram = peak.peak_mb
    vram = vram_mb()

    # Free the TTS model before loading the checking models.
    del ad
    gc.collect()

    progress("Checking voice match…")
    import sherpa_onnx

    spk_path = cache_dir() / "wespeaker_en_voxceleb_resnet34.onnx"
    if not spk_path.exists():
        urllib.request.urlretrieve(SPEAKER_MODEL_URL, spk_path)
    extractor = sherpa_onnx.SpeakerEmbeddingExtractor(
        sherpa_onnx.SpeakerEmbeddingExtractorConfig(model=str(spk_path), num_threads=2)
    )
    ref, ref_sr = read_wav_mono(voice)
    ref_e = speaker_embedding(extractor, ref, ref_sr)
    sims = [float(np.dot(ref_e, speaker_embedding(extractor, a, s))) for _, a, s in outputs]
    voice_match = max(0.0, min(1.0, float(np.mean(sims)))) * 100

    progress("Checking clarity with Whisper…")
    import jiwer
    from faster_whisper import WhisperModel

    wm = WhisperModel(WHISPER_SIZE, device="cpu", compute_type="int8",
                      download_root=str(cache_dir() / "whisper"))
    cers = []
    for text, audio, sr in outputs:
        segs, _ = wm.transcribe(resample(audio, sr, 16000), language=language, beam_size=1)
        hyp = " ".join(s.text for s in segs)
        cers.append(min(1.0, jiwer.cer(normalize(text), normalize(hyp) or "-")))
    clarity = (1.0 - float(np.mean(cers))) * 100

    return {
        "model": model,
        "language": language,
        "speed": round(audio_s / gen_s, 2) if gen_s > 0 else None,
        "ram_mb": ram,
        "vram_mb": vram,
        "voice_match": round(voice_match, 1),
        "clarity": round(clarity, 1),
        "load_seconds": round(load_s, 1),
        "int8": os.environ.get("CORVOX_QUANTIZE", "1") == "1" and model.startswith("pocket"),
    }
