# SPDX-License-Identifier: Apache-2.0
"""Text-to-speech providers behind one ``TTS`` protocol."""
from .base import TTS, Audio, TTSError
from .elevenlabs import ElevenLabsTTS
from .fake import FakeTTS
from .fallback import FallbackTTS

__all__ = ["TTS", "Audio", "TTSError", "ElevenLabsTTS", "FakeTTS", "FallbackTTS"]
