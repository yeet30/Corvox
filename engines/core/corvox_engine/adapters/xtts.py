from __future__ import annotations

from .base import Adapter, to_numpy


class XttsAdapter(Adapter):
    """Coqui XTTS-v2 via the community-maintained `coqui-tts` package.

    The model weights are under the Coqui Public Model License (non-commercial).
    The app only installs this engine after the user agrees, and then sets
    COQUI_TOS_AGREED=1 for this process.
    """

    id = "xtts"
    gpu_capable = True

    def load(self, language=None) -> None:
        from TTS.api import TTS

        self.device = self.pick_device()
        self.tts = TTS("tts_models/multilingual/multi-dataset/xtts_v2").to(self.device)
        self.sr = int(self.tts.synthesizer.output_sample_rate)
        self.loaded = True

    def synth(self, text, voice_path, language):
        lang = "zh-cn" if language == "zh" else language
        wav = self.tts.tts(text=text, speaker_wav=voice_path, language=lang)
        return to_numpy(wav), self.sr
