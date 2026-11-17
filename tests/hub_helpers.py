# SPDX-License-Identifier: Apache-2.0
"""Shared builder for a fully-faked Hub (not collected as tests). Every seam is a deterministic fake, so the
whole 11-stage pipeline runs with no network, no creds.

Time is INJECTED: the webhook fixtures are stamped 2025-11-11, so ``build_hub`` hands the Hub a ``FakeClock``
that starts one minute after the first fixture message. ``deliver`` moves the clock to just after a message's own
timestamp (a message can't arrive before it was sent); tests then ``advance`` it to exercise the 24h window,
approval expiry and ``reopen_days``."""
from __future__ import annotations

import json
from datetime import UTC, datetime, timedelta
from pathlib import Path

from allforone import fakes, judge, kpi, learning, proof_runner, routing
from allforone.config import demo_config
from allforone.helpdesk.fake import FakeHelpdesk
from allforone.helpdesk.gateway import GuardedHelpdesk
from allforone.pipeline import Hub
from allforone.store import MemoryStore

FIX = Path(__file__).parent.parent / "src" / "allforone" / "fixtures"
T0 = datetime.fromtimestamp(1762857600, tz=UTC)        # webhook_text.json timestamp


class FakeClock:
    def __init__(self, now: datetime = T0 + timedelta(minutes=1)):
        self.t = now

    def __call__(self) -> datetime:
        return self.t

    def advance(self, **kw) -> None:
        self.t += timedelta(**kw)

    def at_least(self, t: datetime) -> None:
        if self.t < t:
            self.t = t


def load(name: str) -> dict:
    return json.loads((FIX / name).read_text(encoding="utf-8"))


def build_hub(**over) -> Hub:
    transport = over.pop("transport", None) or FakeHelpdesk()
    cfg = {**demo_config(), "phone_number_id": "PHONE_NUMBER_ID"}
    gw = over.pop("helpdesk", None) or GuardedHelpdesk(transport, writes_on=True, cfg=cfg)
    seams = dict(
        helpdesk=gw,
        whatsapp=fakes.FakeWhatsApp(),
        stt=fakes.FakeSTT(),
        telegram=fakes.FakeTelegram(),
        knowledge=fakes.FakeKnowledge(),
        knowledge_notes=fakes.FakeNotesKnowledge(),
        assigner=routing.RuleAssigner(routing.default_team()),
        proof=proof_runner.FakeProofRunner(),
        kpi_sink=kpi.FakeKpiSink(),
        judge=judge.FakeJudge(),
        learning=learning.FakeLearningLog(),
        cfg=cfg,
        clock=FakeClock(),
        store=MemoryStore(),
    )
    seams.update(over)
    return Hub(**seams)


def _payload_ts(payload: dict) -> datetime | None:
    stamps = [int(m["timestamp"]) for e in payload.get("entry", []) for c in e.get("changes", [])
              for m in (c.get("value") or {}).get("messages", []) if m.get("timestamp")]
    return datetime.fromtimestamp(max(stamps), tz=UTC) if stamps else None


def deliver(hub: Hub, payload: dict | str) -> dict:
    """Deliver a webhook payload (or a fixture name) with the clock just after the message's own timestamp."""
    if isinstance(payload, str):
        payload = load(payload)
    ts = _payload_ts(payload)
    if ts is not None and hasattr(hub.clock, "at_least"):
        hub.clock.at_least(ts + timedelta(minutes=1))
    return hub.handle_webhook(payload)


def msg(mid: str, body: str, *, ts: int = 1762858300, wa_id: str = "15550100001") -> dict:
    """A one-message text payload (customer reply) cloned from the confirm fixture."""
    p = load("webhook_confirm.json")
    m = p["entry"][0]["changes"][0]["value"]["messages"][0]
    m.update({"id": mid, "timestamp": str(ts), "from": wa_id})
    m["text"]["body"] = body
    return p


def pending(hub: Hub, kind: str | None = None) -> dict | None:
    """The latest still-pending approval request the fake Telegram was asked for (optionally of one kind)."""
    for req in reversed(hub.telegram.approvals):
        doc = hub.store.get("approvals", req["nonce"])
        if doc and not doc.get("used_at") and (kind is None or req["kind"] == kind):
            return req
    return None


def approve(hub: Hub, kind: str | None = None, *, as_tg: int | None = None, approved: bool = True,
            text: str | None = None, nonce: str | None = None) -> dict:
    """The owner's tap, as its OWN call after the webhook returned (default: the ticket owner)."""
    req = None
    if nonce is None:
        req = pending(hub, kind)
        assert req is not None, f"no pending approval of kind {kind!r}"
        nonce = req["nonce"]
    tg = as_tg if as_tg is not None else (req["allowed_approvers"][0] if req else 0)
    return hub.on_owner_decision(nonce, tg, approved=approved, text=text)


def run_to_resolved(hub: Hub) -> dict:
    """Happy path: text -> CONFIRM approved -> customer confirms -> owner marks confirmed -> fix ready -> RESOLVE
    approved. Returns the final on_owner_decision result."""
    deliver(hub, "webhook_text.json")
    approve(hub, "confirm")
    deliver(hub, "webhook_confirm.json")
    approve(hub, "advance")
    approve(hub, "fix_ready")
    return approve(hub, "resolve")
