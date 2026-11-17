# SPDX-License-Identifier: Apache-2.0
"""TTS providers: ElevenLabs over an injected HTTP client (no network), the fake, and the fallback chain."""
import pytest

from allforone.tts import TTS, ElevenLabsTTS, FakeTTS, FallbackTTS, TTSError

KEY = "test-key-123"


class Resp:
    def __init__(self, status=200, content=b"ID3audio"):
        self.status_code, self.content = status, content


class Http:
    def __init__(self, resp=None, exc=None):
        self.resp, self.exc, self.calls = resp or Resp(), exc, []

    def post(self, url, headers=None, json=None, timeout=None):
        self.calls.append({"url": url, "headers": headers, "json": json})
        if self.exc:
            raise self.exc
        return self.resp


def make(http, **kw):
    return ElevenLabsTTS("voice-en", voices={"ar": "voice-ar"}, api_key=KEY, client=http, **kw)


def test_request_shape_and_audio():
    http = Http()
    a = make(http).synthesize("hello", "en")
    c = http.calls[0]
    assert c["url"].startswith("https://api.elevenlabs.io/v1/text-to-speech/voice-en")
    assert c["headers"]["xi-api-key"] == KEY
    assert c["json"]["text"] == "hello" and c["json"]["model_id"] == "eleven_multilingual_v2"
    assert (a.data, a.fmt, a.sample_rate) == (b"ID3audio", "mp3", 44100)


def test_voice_selection_order():
    http = Http()
    t = make(http)
    t.synthesize("x", "ar")
    t.synthesize("x", "en", voice="override")
    assert "/voice-ar" in http.calls[0]["url"] and "/override" in http.calls[1]["url"]


def test_from_config_uses_voice_and_model():
    cfg = {"tts": {"voice_id": "v1", "model_id": "m1", "voices": {"ar": "v2"}}}
    t = ElevenLabsTTS.from_config(cfg, api_key=KEY, client=Http())
    assert (t.voice_id, t.model_id, t.voices) == ("v1", "m1", {"ar": "v2"})


def test_key_from_env(monkeypatch):
    monkeypatch.setenv("ELEVENLABS_API_KEY", "env-key")
    http = Http()
    ElevenLabsTTS("v", client=http).synthesize("hi")
    assert http.calls[0]["headers"]["xi-api-key"] == "env-key"


def test_no_key_raises_without_calling(monkeypatch):
    monkeypatch.delenv("ELEVENLABS_API_KEY", raising=False)
    http = Http()
    with pytest.raises(TTSError, match="no ElevenLabs key"):
        ElevenLabsTTS("v", client=http).synthesize("hi")
    assert http.calls == []


@pytest.mark.parametrize("resp,exc", [(Resp(401, b""), None), (Resp(200, b""), None), (None, TimeoutError("x"))])
def test_failures_raise_ttserror_and_never_leak_the_key(resp, exc):
    t = make(Http(resp, exc))
    with pytest.raises(TTSError) as e:
        t.synthesize("hi")
    assert KEY not in str(e.value) and KEY not in repr(t)


def test_empty_text_rejected():
    with pytest.raises(TTSError):
        make(Http()).synthesize("   ")


def test_fake_is_deterministic_wav_and_a_tts():
    f = FakeTTS()
    a, b = f.synthesize("hello world"), f.synthesize("hello world")
    assert a == b and a.fmt == "wav" and a.data[:4] == b"RIFF" and isinstance(f, TTS)


def test_fallback_uses_next_provider_on_failure():
    bad, good = FakeTTS(fail=True), FakeTTS()
    a = FallbackTTS([bad, good]).synthesize("hi", "ar", "v")
    assert a.fmt == "wav" and bad.calls and good.calls == [("hi", "ar", "v")]


def test_fallback_first_success_stops_the_chain():
    first, second = FakeTTS(), FakeTTS()
    FallbackTTS([first, second]).synthesize("hi")
    assert second.calls == []


def test_fallback_all_fail_reports_each_reason():
    with pytest.raises(TTSError, match="all TTS providers failed.*FakeTTS"):
        FallbackTTS([FakeTTS(fail=True), make(Http(Resp(500, b"")))]).synthesize("hi")


def test_fallback_survives_a_buggy_provider():
    class Boom:
        def synthesize(self, *a, **k):
            raise RuntimeError("bug")
    assert FallbackTTS([Boom(), FakeTTS()]).synthesize("hi").fmt == "wav"


def test_fallback_needs_a_chain():
    with pytest.raises(ValueError):
        FallbackTTS([])
