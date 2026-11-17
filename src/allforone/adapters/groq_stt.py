# SPDX-License-Identifier: Apache-2.0
"""Speech-to-text over Groq's whisper-large-v3 (extra: ``pip install allforone[groq]``).

``GroqSTT`` fetches the audio with an injected ``fetch_audio(audio_id) -> bytes`` (for WhatsApp use
``WhatsAppCloud.download_media``), then transcribes it. The Groq client can be injected, so tests never need the
SDK installed or a key. With no key or no SDK it returns ``{"status": "blocked", "why": ...}``.
"""
from __future__ import annotations

import os
from typing import Any, Callable

MODEL = "whisper-large-v3"


class GroqSTT:
    def __init__(self, fetch_audio: Callable[[str], bytes], *, api_key: str = "", client: Any = None,
                 model: str = MODEL):
        self._fetch, self.model = fetch_audio, model
        self._key = api_key or os.environ.get("GROQ_API_KEY", "")
        self._client = client

    def available(self) -> str | None:
        if self._client is not None:
            return None
        if not self._key:
            return "no Groq key (set GROQ_API_KEY); STT blocked"
        try:
            import groq  # noqa: F401
        except ImportError:
            return "the groq package is not installed (pip install allforone[groq]); STT blocked"
        return None

    def _get_client(self):
        if self._client is None:
            from groq import Groq
            self._client = Groq(api_key=self._key)
        return self._client

    def transcribe(self, audio_id: str, *, lang_hint: str = "") -> dict:
        why = self.available()
        if why:
            return {"status": "blocked", "why": why, "text": None}
        try:
            audio = self._fetch(audio_id)
            kw: dict = {"file": (f"{audio_id}.ogg", audio), "model": self.model, "response_format": "json"}
            if lang_hint:
                kw["language"] = lang_hint
            res = self._get_client().audio.transcriptions.create(**kw)
        except Exception as e:                                  # noqa: BLE001 - never crash the pipeline
            return {"status": "blocked", "why": f"transcription failed: {type(e).__name__}", "text": None}
        text = str(getattr(res, "text", "") or (res.get("text") if isinstance(res, dict) else "") or "").strip()
        if not text:
            return {"status": "blocked", "why": "empty transcript", "text": None}
        lang = "ar" if any("؀" <= c <= "ۿ" for c in text) else "en"
        return {"text": text, "lang": lang, "provider": "groq", "model": self.model, "placeholder": False,
                "audio_retained": True}
