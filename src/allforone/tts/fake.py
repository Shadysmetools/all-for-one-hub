# SPDX-License-Identifier: Apache-2.0
"""A deterministic TTS for tests and the demo: no network, no key."""
from __future__ import annotations

import struct

from .base import Audio, TTSError


class FakeTTS:
    """Returns a short WAV tone whose length follows the text. ``fail=True`` makes it raise, to test fallbacks."""

    def __init__(self, *, fail: bool = False, sample_rate: int = 8000):
        self.fail, self.sample_rate = fail, sample_rate
        self.calls: list[tuple[str, str, str | None]] = []

    def synthesize(self, text: str, lang: str = "en", voice: str | None = None) -> Audio:
        self.calls.append((text, lang, voice))
        if self.fail:
            raise TTSError("fake TTS set to fail")
        n = self.sample_rate * max(1, min(len(text) // 15, 10)) // 4
        pcm = b"".join(struct.pack("<h", 3000 if (i // 9) % 2 else -3000) for i in range(n))
        hdr = (b"RIFF" + struct.pack("<I", 36 + len(pcm)) + b"WAVEfmt " + struct.pack("<IHHIIHH", 16, 1, 1, self.sample_rate,
               self.sample_rate * 2, 2, 16) + b"data" + struct.pack("<I", len(pcm)))
        return Audio(hdr + pcm, "wav", self.sample_rate)
