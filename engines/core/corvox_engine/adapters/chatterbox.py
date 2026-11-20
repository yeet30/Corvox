from __future__ import annotations

from .base import Adapter, to_numpy


class ChatterboxAdapter(Adapter):
    """Resemble AI Chatterbox Multilingual (MIT). Best on a GPU."""

    id = "chatterbox"
    gpu_capable = True

    def load(self, language=None) -> None:
        from chatterbox.mtl_tts import ChatterboxMultilingualTTS

        self.device = self.pick_device()
        self.model = ChatterboxMultilingualTTS.from_pretrained(device=self.device)
        self.sr = int(self.model.sr)
        self.loaded = True

    def synth(self, text, voice_path, language):
        wav = self.model.generate(text, language_id=language, audio_prompt_path=voice_path)
        return to_numpy(wav), self.sr
