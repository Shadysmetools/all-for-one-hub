# SPDX-License-Identifier: Apache-2.0
"""The seams. Every external system the hub touches is a small Protocol here, with a FAKE in ``fakes.py`` and,
where one exists, a real adapter under ``channels/`` or ``adapters/`` that blocks itself (plain reason) when its
key or dependency is missing. Tests and the demo inject the fakes; the whole pipeline runs with zero credentials.

A Protocol (structural typing) keeps the hub decoupled: any object with these methods qualifies, so the real
adapters, the fakes and your own implementations are interchangeable without a base-class import.
"""
from __future__ import annotations

from typing import Any, Protocol, runtime_checkable


@runtime_checkable
class WhatsAppGateway(Protocol):
    """Outbound WhatsApp (the hub is the ONLY sender). Every send is gated on the 24h customer-service window."""

    def send_text(self, to: str, text: str, *, last_inbound_at: Any, now: Any, automatic: bool = False) -> dict: ...


@runtime_checkable
class SpeechToText(Protocol):
    """Speech-to-text for voice notes. Real impl blocks without a key; the fake returns a scripted transcript."""

    def transcribe(self, audio_id: str, *, lang_hint: str = "") -> dict: ...


@runtime_checkable
class HelpdeskGateway(Protocol):
    """Guarded helpdesk writes (create / assign / note), all passing ``helpdesk/guard.py``, mail suppressed on
    create."""

    def create_ticket(self, vals: dict, *, context: dict) -> int: ...
    def assign(self, tid: int, user_id: int) -> None: ...
    def post_note(self, tid: int, body: str) -> None: ...


@runtime_checkable
class TelegramGateway(Protocol):
    """Per-ticket topic + the owner-approval channel. Approval is ASYNCHRONOUS and BOUND:

    ``request_approval`` only posts the request (inline buttons carrying ``nonce``) and returns
    ``{"status": "pending"}``. The tap comes back later as ``Hub.on_owner_decision(nonce, approver_tg_id, ...)``.
    The hub is the source of truth for validity: an approval is bound to ``(ticket_id, kind, draft_hash, nonce)``,
    to the approvers in ``allowed_approvers`` (the ticket owner's tg_id + a configured allowlist), to
    ``expires_at``, and is single use. A real gateway must pass the tapping user's Telegram id
    (callback_query.from.id) through unchanged, for approvals AND for ``Hub.reassign(..., actor_tg_id=)`` /
    ``Hub.release_hold(..., actor_tg_id=)`` (owner or allowlist only)."""

    def open_topic(self, tid: int, owner: "Any", brief: str) -> int: ...
    def post(self, topic_id: int, text: str) -> None: ...
    def post_triage(self, text: str) -> None: ...
    def request_approval(self, topic_id: int, *, ticket_id: int, kind: str, draft: str, nonce: str,
                         draft_hash: str, allowed_approvers: list[int], expires_at: "Any") -> dict: ...


@runtime_checkable
class KnowledgeSource(Protocol):
    """A read-only customer-knowledge source. Any number of these feed ANALYSE / routing / the owner brief."""

    def partner_for(self, wa_id: str) -> int | None: ...
    def client_context(self, partner_id: int) -> dict: ...


@runtime_checkable
class ProofRunner(Protocol):
    """PROVE-IT (stage 9): produce evidence that the fix works (e.g. a short proof video + a share link)."""

    def prove(self, request: dict, packet: dict, cfg: dict) -> dict: ...


@runtime_checkable
class KpiSink(Protocol):
    """KPIs (stage 11): push the resolved ticket's outcome and metrics somewhere (a file, a dashboard, stdout)."""

    def push_kpis(self, outcome: dict) -> dict: ...


@runtime_checkable
class LearningLog(Protocol):
    """Append-only learning record on close, plus a periodic review job that proposes (never auto-applies)."""

    def record(self, signals: dict) -> dict: ...
    def review(self) -> dict: ...


@runtime_checkable
class TeamAwareAssigner(Protocol):
    """Pick the owner from role / skills / history / load, with an "assigned to X because..." rationale."""

    def assign(self, analysis: "Any", client_context: dict) -> dict: ...


@runtime_checkable
class Judge(Protocol):
    """An INDEPENDENT model (distinct from the drafter) that scores a draft / classification / assignment / proof
    before the owner sees it, returning per-dimension scores and a low-score flag."""

    def score(self, kind: str, item: dict) -> dict: ...


@runtime_checkable
class EvalHarness(Protocol):
    """Load the golden set, run classify / route / draft, score vs ground truth, fail the gate on regression."""

    def run_gate(self, baseline: dict | None = None) -> dict: ...
