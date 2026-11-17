# SPDX-License-Identifier: Apache-2.0
"""Text-to-speech interface: ``synthesize(text, lang, voice) -> Audio``."""
from __future__ import annotations

from dataclasses import dataclass
from typing import Protocol, runtime_checkable


class TTSError(Exception):
    """A provider could not produce audio. The message never contains credentials."""


@dataclass(frozen=True)
class Audio:
    data: bytes
    fmt: str            # "mp3", "wav", ...
    sample_rate: int    # Hz

    def __len__(self) -> int:
        return len(self.data)


@runtime_checkable
class TTS(Protocol):
    def synthesize(self, text: str, lang: str = "en", voice: str | None = None) -> Audio:
        """Return spoken audio for ``text``. ``voice`` overrides the provider's default voice for ``lang``."""
        ...
