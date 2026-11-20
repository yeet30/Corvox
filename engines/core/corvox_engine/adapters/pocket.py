from __future__ import annotations

import os

from .base import Adapter, to_numpy

# Kyutai ships one model per language. The "_24l" configs are the bigger 24-layer versions.
POCKET_LANGS = {
    "en": "english", "fr": "french", "de": "german", "pt": "portuguese",
    "it": "italian", "es": "spanish", "nl": "dutch",
}
POCKET_LARGE_LANGS = {
    "en": "english_2026-09_24l", "fr": "french_24l", "de": "german_24l", "pt": "portuguese_24l",
    "it": "italian_24l", "es": "spanish_24l", "nl": "dutch_24l",
}
TURKISH_REPO = "kaanhgunay/pocket-tts-tr"
TURKISH_COMMIT = "e5aa490d9aa6075cf047e4286fb57cecb99aa1ed"  # v0.1 base, 2026-09-02


class PocketAdapter(Adapter):
    """Kyutai Pocket TTS on CPU. Loads the model for each language on first use."""

    id = "pocket"
    langs = POCKET_LANGS

    def __init__(self) -> None:
        super().__init__()
        self.models = {}
        self.voices = {}

    def _model_kwargs(self, language: str) -> dict:
        if language not in self.langs:
            raise ValueError(f"{self.id} doesn't speak {language!r}")
        return {"language": self.langs[language]}

    def _model(self, language: str):
        m = self.models.get(language)
        if m is None:
            from pocket_tts import TTSModel

            quantize = os.environ.get("CORVOX_QUANTIZE", "1") == "1"
            m = TTSModel.load_model(**self._model_kwargs(language), quantize=quantize)
            if not getattr(m, "has_voice_cloning", True):
                raise RuntimeError(
                    "Kyutai's voice-cloning model needs a free Hugging Face login: accept the terms at "
                    "huggingface.co/kyutai/pocket-tts, then paste a token in Settings."
                )
            self.models[language] = m
        return m

    def load(self, language: str | None = None) -> None:
        import pocket_tts  # noqa: F401  importing it forces torch to 1 thread, so import first...
        import torch

        # ...then set ours. 4 threads measured fastest on an 8-core Ryzen (1 thread: ~30% slower).
        default = max(1, min(4, os.cpu_count() or 2))
        torch.set_num_threads(int(os.environ.get("CORVOX_THREADS", default)))
        self._model(language or next(iter(self.langs)))
        self.loaded = True

    def _voice(self, model, language: str, voice_path: str):
        key = (language, voice_path, os.path.getmtime(voice_path))
        st = self.voices.get(key)
        if st is None:
            st = model.get_state_for_audio_prompt(voice_path, truncate=True)
            self.voices = {k: v for k, v in self.voices.items() if k[0] != language}
            self.voices[key] = st
        return st

    def synth(self, text, voice_path, language):
        model = self._model(language)
        state = self._voice(model, language, voice_path)
        audio = model.generate_audio(state, text)
        return to_numpy(audio), int(model.sample_rate)

    def warm(self, voice_path, language):
        self._voice(self._model(language), language, voice_path)

    def synth_stream(self, text, voice_path, language, stop):
        # Pocket TTS streams ~80 ms frames itself (first audio in ~0.3 s) and splits sentences.
        model = self._model(language)
        state = self._voice(model, language, voice_path)
        sr = int(model.sample_rate)
        for chunk in model.generate_audio_stream(state, text, stop=stop):
            yield to_numpy(chunk), sr


class PocketLargeAdapter(PocketAdapter):
    id = "pocket-large"
    langs = POCKET_LARGE_LANGS


def turkish_config_path() -> str:
    """The model card's `@v0.1-base` tag doesn't exist on the Hub (only `main`), and the
    config itself points at that tag. Download the config at a pinned commit and point it
    at the same commit."""
    from pathlib import Path

    from huggingface_hub import hf_hub_download

    src = hf_hub_download(TURKISH_REPO, "config.yaml", revision=TURKISH_COMMIT)
    text = Path(src).read_text(encoding="utf-8").replace("@v0.1-base", f"@{TURKISH_COMMIT}")
    out = Path.home() / ".cache" / "corvox" / f"pocket-tr-{TURKISH_COMMIT[:8]}.yaml"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(text, encoding="utf-8")
    return str(out)


class PocketTurkishAdapter(PocketAdapter):
    id = "pocket-tr"
    langs = {"tr": TURKISH_REPO}

    def _model_kwargs(self, language: str) -> dict:
        if language != "tr":
            raise ValueError("Pocket TTS Turkish only speaks Turkish")
        return {"config": turkish_config_path()}
