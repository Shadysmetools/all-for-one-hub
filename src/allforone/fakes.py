# SPDX-License-Identifier: Apache-2.0
"""Deterministic FAKES for every hub seam. Injected into ``Hub`` by tests; the whole 11-stage pipeline runs with
no network, no credentials, no browser, no ffmpeg. Each fake RECORDS what it was asked to do, so tests can assert
things like "exactly one automatic client message" or "zero sends without owner approval".

These are NEVER a production fallback: real adapters block themselves with a plain reason when their
key/dependency is missing.
"""
from __future__ import annotations

from datetime import UTC, datetime

from .channels import whatsapp_cloud
from .model import Owner

# ---------------------------------------------------------------- WhatsApp (outbound only; the hub is sole sender)

class FakeWhatsApp:
    """Records every outbound text. Enforces the 24h customer-service window via the REAL
    ``whatsapp_cloud.in_service_window`` (reuse, not a reimplementation). ``automatic`` marks the auto-ack so a
    test can assert the ack is the ONLY automatic client message."""

    def __init__(self, *, window_hours: float = 24):
        self.sent: list[dict] = []
        self.window_hours = window_hours

    def send_text(self, to: str, text: str, *, last_inbound_at, now=None, automatic: bool = False) -> dict:
        now = now or datetime.now(UTC)
        if not whatsapp_cloud.in_service_window(last_inbound_at, now, self.window_hours):
            raise whatsapp_cloud.WindowClosed("outside the 24h window: only an approved template may be sent")
        rec = {"to": to, "text": text, "automatic": bool(automatic), "ts": now}
        self.sent.append(rec)
        return rec

    def automatic_count(self) -> int:
        return sum(1 for s in self.sent if s["automatic"])


# ---------------------------------------------------------------- STT

class FakeSTT:
    """Returns a scripted transcript fixture (no live speech-to-text call in CI or the demo). Marks the result as
    a PLACEHOLDER because the audio is a synthetic stand-in."""

    def __init__(self, transcripts: dict[str, str] | None = None):
        # media_id -> transcript. Default covers the voice fixture's media id.
        self.transcripts = transcripts or {
            "MEDIA_AUDIO_0001": "عندي مشكلة في المخزون، الكمية ظاهرة غلط بعد ما عملت تسليم",
        }
        self.calls: list[str] = []

    def transcribe(self, audio_id: str, *, lang_hint: str = "") -> dict:
        self.calls.append(audio_id)
        text = self.transcripts.get(audio_id, "(no transcript fixture for this clip)")
        return {"text": text, "lang": "ar" if any("؀" <= c <= "ۿ" for c in text) else "en",
                "provider": "fake-stt (fixture)", "model": "fixture",
                "placeholder": True, "audio_retained": True}


# ---------------------------------------------------------------- Telegram (per-ticket topic + approval channel)

class FakeTelegram:
    """Opens one topic per ticket and is the owner-approval channel. ``request_approval`` is ASYNCHRONOUS: it only
    RECORDS the request (and returns ``pending``). The owner's tap arrives later, as a separate call to
    ``Hub.on_owner_decision(nonce, approver_tg_id, ...)`` (tests use ``hub_helpers.approve``)."""

    def __init__(self):
        self.topics: dict[int, dict] = {}
        self.posts: list[dict] = []
        self.triage: list[str] = []
        self.approvals: list[dict] = []
        self._next = 5000

    def open_topic(self, tid: int, owner, brief: str) -> int:
        self._next += 1
        name = getattr(owner, "name", str(owner))
        self.topics[self._next] = {"tid": tid, "owner": name, "brief": brief}
        return self._next

    def post(self, topic_id: int, text: str) -> None:
        self.posts.append({"topic_id": topic_id, "text": text})

    def post_triage(self, text: str) -> None:
        self.triage.append(text)

    def request_approval(self, topic_id: int, *, ticket_id: int, kind: str, draft: str, nonce: str,
                         draft_hash: str, allowed_approvers: list[int], expires_at) -> dict:
        req = {"topic_id": topic_id, "ticket_id": ticket_id, "kind": kind, "draft": draft, "nonce": nonce,
               "draft_hash": draft_hash, "allowed_approvers": list(allowed_approvers), "expires_at": expires_at}
        self.approvals.append(req)
        return {"status": "pending", "nonce": nonce}


# ---------------------------------------------------------------- Knowledge sources, read-only

class FakeKnowledge:
    """Stub of the primary customer-knowledge source: sender -> partner, plus client history used by routing."""

    def __init__(self, partners: dict[str, int] | None = None, context: dict[int, dict] | None = None):
        self.partners = partners if partners is not None else {"15550100001": 3001, "15550100002": 3002}
        self.context = context if context is not None else {}

    def partner_for(self, wa_id: str) -> int | None:
        return self.partners.get(str(wa_id))

    def client_context(self, partner_id: int) -> dict:
        return dict(self.context.get(int(partner_id), {"source": "crm", "modules": [], "owner_uids": []}))


class FakeNotesKnowledge:
    """Stub of a second knowledge source (meeting notes, decisions, commitments, usual collaborators per client).
    Read-only."""

    def __init__(self, context: dict[int, dict] | None = None):
        self.context = context or {
            3001: {"source": "notes", "agreed": ["monthly inventory review"],
                   "open_commitments": ["enable lot tracking"], "usual_collaborators": ["Maya"],
                   "modules": ["Inventory"], "owner_uids": [778]},
        }

    def partner_for(self, wa_id: str) -> int | None:   # keyed by partner, resolved via the primary source
        return None

    def client_context(self, partner_id: int) -> dict:
        return dict(self.context.get(int(partner_id), {"source": "notes"}))


def merge_context(*contexts: dict) -> dict:
    """Merge the two knowledge sources into one brief dict (lists concatenated, scalars last-wins)."""
    out: dict = {"modules": [], "owner_uids": [], "agreed": [], "open_commitments": [], "usual_collaborators": [],
                 "type_owner": {}}
    for c in contexts:
        for k, v in (c or {}).items():
            if isinstance(v, list):
                out.setdefault(k, [])
                for item in v:
                    if item not in out[k]:
                        out[k].append(item)
            elif isinstance(v, dict):
                out.setdefault(k, {}).update(v)
            else:
                out[k] = v
    return out


def owner(uid: int, name: str, tg_id: int = 0, role: str = "") -> Owner:
    return Owner(uid=uid, name=name, tg_id=tg_id, role=role)
