"""Synthesize data/voiceover.json with the official ElevenLabs REST API.

Key comes from the ELEVENLABS_API_KEY env var. It is only ever sent in the xi-api-key header, never printed or stored.
MP3s are cached by content hash in CACHE_DIR (default: ./voiceover/.cache), so unchanged
segments are never re-synthesized (and never re-billed).
Writes data/scenes.json (scene durations from the measured audio) and ../demo.srt.
"""
import hashlib
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).parent
CACHE = Path(os.environ.get("CACHE_DIR", HERE / "voiceover"))
CACHE.mkdir(parents=True, exist_ok=True)


def api_key():
    k = os.environ.get("ELEVENLABS_API_KEY")
    if not k:
        sys.exit("Set ELEVENLABS_API_KEY to synthesize new clips (cached clips need no key).")
    return k


def synth(text, voice, model, key):
    body = json.dumps({"text": text, "model_id": model,
                       "voice_settings": {"stability": 0.5, "similarity_boost": 0.75}}).encode()
    req = urllib.request.Request(
        f"https://api.elevenlabs.io/v1/text-to-speech/{voice}?output_format=mp3_44100_128",
        data=body, method="POST",
        headers={"xi-api-key": key, "Content-Type": "application/json", "Accept": "audio/mpeg"})
    try:
        return urllib.request.urlopen(req, timeout=120).read()
    except urllib.error.HTTPError as e:
        sys.exit(f"ElevenLabs error {e.code} (auth/quota?). Stopping: {e.read()[:200]!r}")


def duration(path):
    out = subprocess.check_output(["ffprobe", "-v", "error", "-show_entries", "format=duration",
                                   "-of", "csv=p=0", str(path)])
    return float(out)


def srt_time(s):
    ms = round(s * 1000)
    return f"{ms//3600000:02}:{ms//60000%60:02}:{ms//1000%60:02},{ms%1000:03}"


NL = chr(10)
SRT_FMT = "{i}" + NL + "{a} --> {b}" + NL + "{c}" + NL


def main():
    cfg = json.loads((HERE / "data/voiceover.json").read_text(encoding="utf-8"))
    t, scenes, srt = 0.0, [], []
    state = {"key": None}

    def clip_file(cid, text):
        h = hashlib.sha1((cfg["voice_id"] + cfg["model_id"] + text).encode()).hexdigest()[:12]
        mp3 = CACHE / f"{cid}-{h}.mp3"
        if not mp3.exists():
            state["key"] = state["key"] or api_key()
            mp3.write_bytes(synth(text, cfg["voice_id"], cfg["model_id"], state["key"]))
            print("synthesized", cid, len(text), "chars")
        return mp3

    for i, s in enumerate(cfg["segments"], 1):
        # a segment is one clip (text) or several (clips: gap_ms after the previous clip, cut_ms trimmed off the end)
        clips_in = s.get("clips") or [{"id": s["id"], "text": s["text"], "gap_ms": s.get("lead_ms", cfg["lead_ms"])}]
        cur, clips = 0.0, []
        for c in clips_in:
            mp3 = clip_file(c["id"], c["text"])
            dur = duration(mp3) - c.get("cut_ms", 0) / 1000
            at = cur + c["gap_ms"] / 1000
            clips.append({"id": c["id"], "file": mp3.name, "at": round(at, 3), "dur": round(dur, 3),
                          "start": round(t + at, 3), "cut": c.get("cut_ms", 0) / 1000})
            cur = at + dur
        tail = s.get("tail_ms", cfg["tail_ms"]) / 1000
        total = max(cur + tail, s.get("min_ms", 0) / 1000)
        scenes.append({"id": s["id"], "scene": s["scene"], "start": round(t, 3), "dur": round(total, 3),
                       "audio_start": clips[0]["start"], "audio_dur": clips[0]["dur"],
                       "caption": s["caption"], "clips": clips})
        srt.append(SRT_FMT.format(i=i, a=srt_time(t + 0.1), b=srt_time(t + total - 0.1), c=s["caption"]))
        t += total
    (HERE / "data/scenes.json").write_text(json.dumps({"total": round(t, 3), "scenes": scenes}, indent=1), encoding="utf-8")
    (HERE.parent / "demo.srt").write_text(NL.join(srt), encoding="utf-8")
    print("total", round(t, 1), "s")


main()
