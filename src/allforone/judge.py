# SPDX-License-Identifier: Apache-2.0
"""An INDEPENDENT judge (a different model from the drafter in production) that scores every draft client reply
BEFORE the owner sees it, plus the classification, the assignment and the proof video.

Per-dimension scores (0..1): correctness, tone, language, policy, completeness. A low aggregate raises a FLAG;
the hub routes a flagged item to owner review. Judge scores feed the learning log and the KPI sink, and
``agreement`` computes the judge-vs-human match rate for periodic calibration.

The fake is deterministic (keyword heuristics) so CI needs no model. ``LLMJudge`` wraps any ``complete(prompt)``
callable (see ``adapters/vertex.py``); ``PrimaryFallbackJudge`` chains two judges. ``model`` must differ from the
drafter's model in production config.
"""
from __future__ import annotations

import json
import re
from typing import Callable

from .redact import redact

DIMENSIONS = ("correctness", "tone", "language", "policy", "completeness")
DEFAULT_THRESHOLD = 0.7

# Phrases that violate a polite support style (over-promising, rudeness, missing sign-off).
_BAD_TONE = ("stupid", "obviously", "whatever", "idiot", "غبي", "طبعا انت")
_OVERPROMISE = ("guaranteed", "100%", "forever", "never fail", "مضمون ١٠٠")
SIGN_MARKERS = ("Northwind Supplies Support", "دعم Northwind")


def _lang_of(text: str) -> str:
    """'ar' when Arabic letters are the clear majority of the letters (brand/module/name fragments in Latin are
    tolerated); a mostly-English text with one Arabic character is 'en' ."""
    ar = sum(1 for c in text or "" if "؀" <= c <= "ۿ")
    latin = sum(1 for c in text or "" if c.isascii() and c.isalpha())
    return "ar" if ar and ar >= 0.6 * (ar + latin) else "en"


class FakeJudge:
    """Deterministic independent judge. ``model`` names the (distinct) judge model for the production assertion."""

    def __init__(self, *, model: str = "fake-judge", threshold: float = DEFAULT_THRESHOLD,
                 sign_markers: tuple[str, ...] = SIGN_MARKERS):
        self.model = model
        self.sign_markers = tuple(sign_markers)
        self.threshold = threshold
        self.scored: list[dict] = []

    def score(self, kind: str, item: dict) -> dict:
        if kind == "draft":
            scores = self._score_draft(item)
        elif kind == "classification":
            scores = self._score_classification(item)
        elif kind == "assignment":
            scores = self._score_assignment(item)
        elif kind == "video":
            scores = self._score_video(item)
        else:
            scores = {d: 0.5 for d in DIMENSIONS}
        agg = round(sum(scores.values()) / len(scores), 3)
        res = {"kind": kind, "model": self.model, "scores": scores, "aggregate": agg,
               "flag": agg < self.threshold, "threshold": self.threshold}
        self.scored.append(res)
        return res

    def _score_draft(self, item: dict) -> dict:
        text = str(item.get("text") or "")
        expect_lang = str(item.get("expect_lang") or _lang_of(text))
        low = text.lower()
        correctness = 0.9 if text.strip() else 0.2
        tone = 0.3 if any(b in low for b in _BAD_TONE) else 0.9
        policy = 0.3 if any(o in low for o in _OVERPROMISE) else (0.9 if any(s in text for s in self.sign_markers) else 0.6)
        language = 0.9 if _lang_of(text) == expect_lang else 0.3
        completeness = 0.9 if (len(text.split()) >= 8 and any(s in text for s in self.sign_markers)) else 0.5
        return {"correctness": correctness, "tone": tone, "language": language, "policy": policy,
                "completeness": completeness}

    def _score_classification(self, item: dict) -> dict:
        ok = bool(item.get("ticket_type")) and str(item.get("module")) != ""
        conf = float(item.get("confidence") or 0.5)
        base = 0.85 if ok else 0.4
        return {d: round(min(0.95, base + (conf - 0.5) * 0.2), 3) for d in DIMENSIONS}

    def _score_assignment(self, item: dict) -> dict:
        has_reason = bool(item.get("factors") or item.get("rationale"))
        base = 0.85 if has_reason else 0.4
        return {d: base for d in DIMENSIONS}

    def _score_video(self, item: dict) -> dict:
        prepared = item.get("status") == "prepared" and item.get("verdict") in ("reproduced", "data_only")
        base = 0.88 if prepared else 0.45
        return {d: base for d in DIMENSIONS}


def agreement(pairs: list[tuple[bool, bool]]) -> float:
    """Judge↔human agreement rate from (judge_flag, human_flag) pairs (a common target is >= 0.85)."""
    if not pairs:
        return 0.0
    match = sum(1 for j, h in pairs if bool(j) == bool(h))
    return round(match / len(pairs), 3)


# ---------------------------------------------------------------- live judges (redact first, never fabricate)

def redact_for_judge(text: str, names: list[str] | None = None) -> str:
    """Strip names + phone numbers (and the other secret/PII patterns) BEFORE every judge call. Phones, emails and
    tokens come from the shared ``redact.redact``; names are masked here (the judge never needs them)."""
    out = redact(str(text or ""))
    for n in (names or []):
        n = str(n or "").strip()
        if len(n) >= 2:
            out = re.sub(re.escape(n), "[NAME]", out, flags=re.IGNORECASE)
    return out


def redact_item_for_judge(kind: str, item: dict, names: list[str] | None = None) -> dict:
    """Redact the free-text fields of a judge ``item`` before the call. Scores/flags are unaffected."""
    red = dict(item)
    for f in ("text", "rationale", "note", "understanding", "summary"):
        if isinstance(red.get(f), str):
            red[f] = redact_for_judge(red[f], names)
    return red


PROMPT = ("You are an independent quality judge for customer-support output. Score the item below on each of "
          "these dimensions from 0 to 1: {dims}. Reply with ONLY a JSON object mapping each dimension to a "
          "number.\nKind: {kind}\nItem: {item}")


class LLMJudge:
    """A live judge over any ``complete(prompt) -> str`` callable. Redacts every item first. When ``complete`` is
    missing or fails, it returns ``{"status": "blocked", ...}``: never a fabricated score."""

    def __init__(self, complete: Callable[[str], str] | None = None, *, model: str = "llm-judge",
                 threshold: float = DEFAULT_THRESHOLD, unavailable: str = "no model configured"):
        self.complete, self.model, self.threshold, self.unavailable = complete, model, threshold, unavailable

    def score(self, kind: str, item: dict, *, names: list[str] | None = None) -> dict:
        red = redact_item_for_judge(kind, item, names)        # redact BEFORE any provider call
        blocked = {"kind": kind, "model": self.model, "status": "blocked", "redacted": True}
        if self.complete is None:
            return {**blocked, "why": self.unavailable}
        prompt = PROMPT.format(dims=", ".join(DIMENSIONS), kind=kind, item=json.dumps(red, ensure_ascii=False))
        try:
            raw = json.loads(self._json_part(self.complete(prompt)))
            scores = {d: max(0.0, min(1.0, float(raw[d]))) for d in DIMENSIONS}
        except Exception as e:                                # noqa: BLE001 - a provider/parse error is "blocked"
            return {**blocked, "why": f"judge call failed: {type(e).__name__}"}
        agg = round(sum(scores.values()) / len(scores), 3)
        return {"kind": kind, "model": self.model, "scores": scores, "aggregate": agg,
                "flag": agg < self.threshold, "threshold": self.threshold, "redacted": True}

    @staticmethod
    def _json_part(text: str) -> str:
        m = re.search(r"\{.*\}", str(text), flags=re.S)
        return m.group(0) if m else "{}"


class PrimaryFallbackJudge:
    """Try the primary judge; if it is blocked, fall back. Always redacts first (each judge does)."""

    def __init__(self, primary, fallback, *, model: str | None = None):
        self.primary = primary
        self.fallback = fallback
        self.model = model or getattr(primary, "model", "judge")

    def score(self, kind: str, item: dict, *, names: list[str] | None = None) -> dict:
        res = self.primary.score(kind, item, names=names)
        if res.get("status") == "blocked":
            fb = self.fallback.score(kind, item, names=names)
            fb["via"] = "fallback"
            return fb
        res["via"] = "primary"
        return res


def resolve_judge(cfg: dict | None = None, *, primary_complete=None, fallback_complete=None):
    """Build the primary + fallback judge from ``cfg['judge']`` (``primary_model`` / ``fallback_model``). The
    ``complete`` callables come from an adapter (e.g. ``adapters.vertex.vertex_complete``); without them each judge
    reports ``blocked`` and the hub treats the draft as UNJUDGED. CI injects ``FakeJudge`` instead."""
    j = (cfg or {}).get("judge") or {}
    primary = LLMJudge(primary_complete, model=str(j.get("primary_model") or "primary-model"),
                       unavailable="primary judge not configured")
    fallback = LLMJudge(fallback_complete, model=str(j.get("fallback_model") or "fallback-model"),
                        unavailable="fallback judge not configured")
    return PrimaryFallbackJudge(primary, fallback)
