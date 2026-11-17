# SPDX-License-Identifier: Apache-2.0
"""Try providers in order; the first that produces audio wins."""
from __future__ import annotations

from collections.abc import Sequence

from .base import TTS, Audio, TTSError


class FallbackTTS:
    def __init__(self, chain: Sequence[TTS]):
        if not chain:
            raise ValueError("FallbackTTS needs at least one provider")
        self.chain = list(chain)

    def synthesize(self, text: str, lang: str = "en", voice: str | None = None) -> Audio:
        why: list[str] = []
        for p in self.chain:
            try:
                return p.synthesize(text, lang, voice)
            except TTSError as e:
                why.append(f"{type(p).__name__}: {e}")
            except Exception as e:                              # noqa: BLE001 - a provider bug must not stop the chain
                why.append(f"{type(p).__name__}: {type(e).__name__}")
        raise TTSError("all TTS providers failed: " + "; ".join(why))
