# SPDX-License-Identifier: Apache-2.0
"""ElevenLabs text-to-speech over the official REST API (``POST /v1/text-to-speech/{voice_id}``).

Use stock (premade) voices; cloned voices belong to real people and need their consent. The API key comes from
the ``ELEVENLABS_API_KEY`` environment variable (or the ``api_key`` argument) and is only ever sent in the
``xi-api-key`` header: it is never logged, put in an error message or shown by ``repr``.
The HTTP client is injectable (anything with ``post(url, headers=, json=, timeout=)``), so tests need no network.
"""
from __future__ import annotations

import os
from typing import Any

from .base import Audio, TTSError

BASE_URL = "https://api.elevenlabs.io"
DEFAULT_MODEL = "eleven_multilingual_v2"
OUTPUT_FORMAT = "mp3_44100_128"


class ElevenLabsTTS:
    def __init__(self, voice_id: str, *, model_id: str = DEFAULT_MODEL, voices: dict[str, str] | None = None,
                 api_key: str = "", client: Any = None, base_url: str = BASE_URL, timeout: float = 60.0,
                 stability: float = 0.5, similarity_boost: float = 0.75):
        self.voice_id, self.model_id, self.voices = voice_id, model_id, dict(voices or {})
        self._key = api_key or os.environ.get("ELEVENLABS_API_KEY", "")
        self._client, self._base, self._timeout = client, base_url.rstrip("/"), timeout
        self._settings = {"stability": stability, "similarity_boost": similarity_boost}

    def __repr__(self) -> str:                                  # never show the key
        return f"ElevenLabsTTS(voice_id={self.voice_id!r}, model_id={self.model_id!r})"

    @classmethod
    def from_config(cls, cfg: dict, **kw: Any) -> ElevenLabsTTS:
        c = (cfg or {}).get("tts") or {}
        return cls(c["voice_id"], model_id=c.get("model_id", DEFAULT_MODEL), voices=c.get("voices"), **kw)

    def _http(self) -> Any:
        if self._client is None:
            import httpx
            self._client = httpx.Client()
        return self._client

    def synthesize(self, text: str, lang: str = "en", voice: str | None = None) -> Audio:
        if not self._key:
            raise TTSError("no ElevenLabs key (set ELEVENLABS_API_KEY)")
        if not (text or "").strip():
            raise TTSError("empty text")
        vid = voice or self.voices.get(lang) or self.voice_id
        try:
            r = self._http().post(
                f"{self._base}/v1/text-to-speech/{vid}?output_format={OUTPUT_FORMAT}", timeout=self._timeout,
                headers={"xi-api-key": self._key, "Content-Type": "application/json", "Accept": "audio/mpeg"},
                json={"text": text, "model_id": self.model_id, "voice_settings": self._settings})
        except Exception as e:                                  # noqa: BLE001 - provider failure, not a crash
            raise TTSError(f"ElevenLabs request failed: {type(e).__name__}") from None
        if r.status_code != 200:
            raise TTSError(f"ElevenLabs returned HTTP {r.status_code}")
        if not r.content:
            raise TTSError("ElevenLabs returned no audio")
        return Audio(bytes(r.content), "mp3", 44100)
