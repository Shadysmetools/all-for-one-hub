# SPDX-License-Identifier: Apache-2.0
"""The owner side of the hub asynchronous, BOUND approvals and everything
that happens after an owner decision. Mixed into ``Hub`` (``pipeline.py``), which owns the store/clock/seams.

Approval kinds:
  confirm   -> sends the CONFIRM draft (Template 2/2b) to the customer
  advance   -> NO client message: the owner marks the customer's reply as the confirmation ("work starts")
  fix_ready -> NO client message: the owner says the fix is done, so PROVE-IT may run (once)
  resolve   -> sends the RESOLVE draft (Template 3b) and closes the ticket

An approval is bound to ``(ticket, kind, sha256(draft), nonce)``, to the approvers allowed for that ticket (the
owner's tg_id + ``cfg.approver_tg_ids``; tg_id 0 never counts), has an expiry, and is single use. The hub keeps the
draft itself and sends THAT (never text echoed back in a callback), except an owner EDIT, which is re-scanned
(``redact.scan``) and re-judged before it can be sent.
"""
from __future__ import annotations

import hashlib
import html
import logging
import re
import secrets
import uuid
from datetime import datetime, timedelta

from . import judge as judge_mod
from . import redact
from .channels import whatsapp_cloud
from .model import TicketState

log = logging.getLogger("allforone")

CLIENT_KINDS = ("confirm", "resolve", "reply", "status")      # kinds that send to the customer (24h approval TTL)
SIDE_KINDS = ("status",)                              # owner status updates: never touch the stage / live approval
INTERNAL_KINDS = ("advance", "fix_ready", "reopen")  # owner-only steps: long TTL, never tied to the 24h window
RESOLUTION_VERDICTS = ("reproduced", "data_only")   # a proof video that SHOWS it working -> Template 3b


def sha(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _iso(dt: datetime) -> str:
    return dt.isoformat()


class OwnerFlow:
    # ---------------------------------------------------------------- small shared helpers

    def _note(self, tid: int, body: str) -> None:
        """Best-effort internal note: a failed note must never break (or repeat) a client-facing step."""
        try:
            self.helpdesk.post_note(tid, body)
        except Exception as e:                               # noqa: BLE001
            log.error("odoo note failed on ticket %s: %s", tid, type(e).__name__)

    def _post(self, state: TicketState, text: str) -> None:
        """Best-effort topic post: a Telegram hiccup must never strand a ticket mid-transition."""
        if state.topic_id:
            try:
                self.telegram.post(state.topic_id, text)
            except Exception as e:                           # noqa: BLE001
                log.error("topic post failed on ticket %s: %s", state.tid, type(e).__name__)

    def _judge(self, kind: str, item: dict) -> dict:
        """Normalise ANY judge outcome (real judge blocked/erroring, fake) to {aggregate, flag, unjudged}: a judge
        that cannot score never crashes the pipeline; the item goes to the owner flagged as UNJUDGED."""
        try:
            res = self.judge.score(kind, item)
        except Exception as e:                               # noqa: BLE001
            return {"kind": kind, "aggregate": None, "flag": True, "unjudged": True, "why": type(e).__name__}
        if not isinstance(res, dict) or res.get("status") == "blocked" or res.get("aggregate") is None:
            why = res.get("why") if isinstance(res, dict) else "bad result"
            return {"kind": kind, "aggregate": None, "flag": True, "unjudged": True, "why": why}
        return {**res, "flag": bool(res.get("flag")), "unjudged": False}

    def _judge_draft(self, state: TicketState, text: str) -> dict:
        # the draft may carry the proof share_link, which embeds the client name: mask both and drop URLs
        red = judge_mod.redact_for_judge(text, names=[state.inbound.name, state.client])
        red = re.sub(r"https?://\S+", "[LINK]", red)
        return self._judge("draft", {"text": red, "expect_lang": state.lang})

    def _approvers(self, state: TicketState) -> list[int]:
        ids = {int(t) for t in (self.cfg.get("approver_tg_ids") or []) if int(t) > 0}
        if state.owner and int(state.owner.tg_id or 0) > 0:
            ids.add(int(state.owner.tg_id))
        return sorted(ids)

    def _actor_ok(self, actor, state: TicketState | None = None) -> bool:
        """Who may reassign / release a hold: the ticket's CURRENT owner or the configured allowlist. Fail closed on
        None/0/non-int. A real gateway must pass the tapping user's Telegram id (callback_query.from.id) unchanged."""
        try:
            a = int(actor)
        except (TypeError, ValueError):
            return False
        if a <= 0:
            return False
        ids = {int(t) for t in (self.cfg.get("approver_tg_ids") or []) if int(t) > 0}
        if state is not None and state.owner and int(state.owner.tg_id or 0) > 0:
            ids.add(int(state.owner.tg_id))
        return a in ids

    def _window_open(self, state: TicketState, now: datetime) -> bool:
        if not state.last_inbound_at:
            return False
        last = min(datetime.fromisoformat(state.last_inbound_at), now)   # a message can't be from the future
        return whatsapp_cloud.in_service_window(last, now, float(self.cfg.get("window_hours", 24)))

    # ---------------------------------------------------------------- holds (the human queue)

    def _hold(self, reason: str, *, inbound=None, ticket_id: int = 0, detail: str = "", log_: list | None = None,
              message_id: str = "") -> dict:
        hid = uuid.uuid4().hex[:8]
        doc = {"id": hid, "reason": reason, "status": "open", "ticket_id": ticket_id, "detail": detail,
               "created_at": _iso(self.clock()), "message_id": (inbound.message_id if inbound else message_id),
               "inbound": inbound.to_dict() if inbound else None}
        self.store.put("holds", hid, doc)
        wa = inbound.wa_id if inbound else ""
        who = f" sender=...{wa[-4:]}" if wa else ""
        kind = f" kind={inbound.kind}" if inbound else ""
        try:
            self.telegram.post_triage(f"HOLD [{reason}] ticket={ticket_id or '-'}{who}{kind} hold_id={hid} {detail}".strip())
        except Exception as e:                               # noqa: BLE001
            log.error("triage post failed: %s", type(e).__name__)
        res = {"held": reason, "hold_id": hid}
        if log_ is not None:
            res["log"] = log_
        return res

    def _triage(self, text: str) -> None:
        try:
            self.telegram.post_triage(text)
        except Exception as e:                               # noqa: BLE001
            log.error("triage post failed: %s", type(e).__name__)

    def holds(self, status: str = "open") -> list[dict]:
        return [h for h in self.store.scan("holds") if h.get("status") == status]

    def _close_holds(self, *, message_id: str = "", ticket_id: int = 0, reason: str = "") -> None:
        for h in self.holds():
            same = (message_id and h.get("message_id") == message_id) or (ticket_id and h.get("ticket_id") == ticket_id)
            if same and (not reason or h["reason"] == reason):
                self.store.update("holds", h["id"], lambda d: {**d, "status": "closed"})

    def release_hold(self, hold_id: str, *, actor_tg_id=None) -> dict:
        """A human fixed the cause (e.g. created the res.partner): re-drive the held message through the pipeline."""
        hold = self.store.get("holds", hold_id)
        if not hold or hold.get("status") != "open":
            return {"released": False, "reason": "unknown or already closed"}
        mdoc = self.store.get("messages", hold.get("message_id") or "") if hold.get("message_id") else None
        raw = hold.get("inbound") or (mdoc or {}).get("inbound")
        from .model import Inbound
        inbound = Inbound.from_dict(raw) if raw else None
        state = self.ticket(hold["ticket_id"]) if hold.get("ticket_id") else None
        with self._wa_lock(inbound.wa_id if inbound else (state.wa_id if state else "")):
            if not self._actor_ok(actor_tg_id, state):
                return {"released": False, "reason": "not_authorised"}
            self.store.update("holds", hold_id, lambda d: {**d, "status": "released"})
            if inbound is None:
                return {"released": True}
            self.store.update("messages", hold["message_id"], lambda d: {**d, "status": "failed", "next_at": ""})
            return self._handle_one(inbound)

    # ---------------------------------------------------------------- issuing approvals

    def _ttl(self, kind: str) -> timedelta:
        if kind in INTERNAL_KINDS:                           # owner-only steps outlive the 24h customer window
            return timedelta(days=float(self.cfg.get("internal_approval_ttl_days", 14)))
        return timedelta(hours=float(self.cfg.get("approval_ttl_hours", 24)))

    def _request_approval(self, state: TicketState, kind: str, draft: str, *, judged: bool | None = None) -> str:
        """Create ONE live, bound approval for the ticket (older ones are superseded) and ask the owner.

        The state is saved ONLY after Telegram accepted the request: if Telegram fails, the stored ticket still
        shows the earlier stage, so a retry of the message re-asks the owner instead of stranding the ticket."""
        judged = kind in CLIENT_KINDS if judged is None else judged
        side = kind in SIDE_KINDS
        now = self.clock()
        nonce = secrets.token_urlsafe(16)
        allowed = self._approvers(state)
        exp = now + self._ttl(kind)
        j = self._judge_draft(state, draft) if judged else None
        self.store.put("approvals", nonce, {
            "nonce": nonce, "ticket_id": state.tid, "kind": kind, "draft": draft, "draft_hash": sha(draft),
            "allowed": allowed, "issued_at": _iso(now), "expires_at": _iso(exp), "used_at": None, "used_by": None,
            "approved": None, "judge": j["aggregate"] if j else None})
        try:
            if j is not None:
                score = "UNJUDGED" if j["unjudged"] else j["aggregate"]
                flag = (" | UNJUDGED: owner-review" if j["unjudged"] else " | LOW SCORE: owner-review") if j["flag"] else ""
                self._post(state, f"[{kind} draft | judge {score}{flag}]\n{draft}")
            self.telegram.request_approval(state.topic_id, ticket_id=state.tid, kind=kind, draft=draft, nonce=nonce,
                                           draft_hash=sha(draft), allowed_approvers=allowed, expires_at=exp)
        except BaseException:
            self.store.update("approvals", nonce, lambda d: {**d, "used_at": "request_failed"})
            raise
        if side:                       # a status update is a SIDE message: the ticket's own approval stays live
            state.side_nonces.append(nonce)
            self._save(state)
            return nonce
        if state.pending_nonce:
            self.store.update("approvals", state.pending_nonce,
                              lambda d: d if d.get("used_at") else {**d, "used_at": "superseded"})
        state.pending_nonce, state.pending_kind, state.declined = nonce, kind, False
        self._save(state)
        return nonce

    def _live_pending(self, state: TicketState) -> bool:
        if not state.pending_nonce:
            return False
        doc = self.store.get("approvals", state.pending_nonce)
        return bool(doc and not doc.get("used_at") and datetime.fromisoformat(doc["expires_at"]) > self.clock())

    def _confirm_draft(self, state: TicketState) -> str:
        a = state.analysis
        return self.tpl.confirm(state.lang, name=state.inbound.name or "there", issue=a.summary, module=a.module,
                                 outcome=a.desired_outcome, questions=a.open_questions)

    def _resolve_draft(self, state: TicketState) -> str | None:
        link = state.proof.get("share_link")
        if not link:
            return None
        what = f"{self.tpl.module_label(state.lang, state.analysis.module)}: {state.analysis.summary}"
        return self.tpl.resolve(state.lang, name=state.inbound.name or "there", n=state.tid, what=what, link=link,
                                 video=link)

    def _ensure_pending(self, state: TicketState, *, force: bool = False, internal_only: bool = False) -> bool:
        """Give the ticket a WAY FORWARD: if it is waiting on the owner but has no live approval (expired, declined,
        window-closed, Telegram failure), ask again for the step the ticket is actually in. ``force`` (a customer
        message, an owner re-ask) overrides a decline; the scheduler tick passes ``internal_only`` so a client-facing
        draft is never re-asked on a timer. Never re-asks a send whose outcome is unknown."""
        if self._interrupted(state) or self._live_pending(state) or (state.declined and not force):
            return False
        st, a = state.stage, state.analysis
        if st == "awaiting_owner" and not internal_only:
            self._request_approval(state, "confirm", self._confirm_draft(state))
        elif st == "awaiting_owner_advance" and state.replies:
            self._request_approval(state, "advance", state.replies[-1]["text"], judged=False)
        elif st == "in_progress" and not state.needs_owner_text and a.ticket_type != "Inquiries":
            self._request_approval(state, "fix_ready", f"Fix ready for ticket #{state.tid}? Approve to run PROVE-IT.",
                                   judged=False)
        elif st == "awaiting_owner_resolve" and not internal_only:
            draft = self._resolve_draft(state)
            if draft is None:
                self._request_approval(state, "fix_ready", f"Retry PROVE-IT for ticket #{state.tid}?", judged=False)
            else:
                self._request_approval(state, "resolve", draft)
        else:
            return False
        return True

    def _expire_side(self, state: TicketState) -> None:
        """A status update describes the ticket AS IT WAS: it dies when the ticket resolves, closes or reopens."""
        for n in state.side_nonces:
            self.store.update("approvals", n, lambda d: d if d.get("used_at") else {**d, "used_at": "superseded"})
        state.side_nonces = []

    def _interrupted(self, state: TicketState) -> bool:
        """True while a client send's outcome is UNKNOWN. A leftover ``attempting_kind`` (the process died between
        recording the attempt and the result) is turned into ``send_unknown``: never re-offered or re-sent; only
        ``confirm_unknown_send`` clears it (the same rule the ack applies to ``7-ACK(attempting)``)."""
        if state.attempting_kind and not state.send_unknown:
            state.send_unknown, state.attempting_kind = state.attempting_kind, ""
            self._save(state)
            self._hold("send_outcome_unknown", ticket_id=state.tid,
                       detail="an interrupted send: check WhatsApp manually; NOT resent")
        return bool(state.send_unknown)

    def _safe_ensure(self, tid: int) -> None:
        try:
            state = self.ticket(tid)
            if state is not None:
                self._ensure_pending(state, force=True)
        except Exception as e:                               # noqa: BLE001 - e.g. Telegram still down: tick retries
            log.error("re-ask failed on ticket %s: %s", tid, type(e).__name__)

    def _reissue_pending(self, state: TicketState) -> None:
        """Re-ask the owner for the SAME draft (new nonce, current approvers), e.g. after a reassignment."""
        if not state.pending_nonce:
            return
        doc = self.store.get("approvals", state.pending_nonce)
        if doc and not doc.get("used_at"):
            self._request_approval(state, doc["kind"], doc["draft"])

    # ---------------------------------------------------------------- the owner's tap

    def _consume(self, nonce: str, approver: int, approved: bool, now: datetime):
        verdict: dict = {}

        def fn(doc: dict) -> dict:
            if int(approver or 0) <= 0 or int(approver) not in doc.get("allowed", []):
                verdict["reason"] = "not_authorised"          # a refused tap does NOT burn the nonce
            elif doc.get("used_at") == "expired":
                verdict["reason"] = "expired"
            elif doc.get("used_at"):
                verdict["reason"] = "used"
            elif datetime.fromisoformat(doc["expires_at"]) <= now:
                verdict["reason"] = "expired"
            elif sha(doc["draft"]) != doc["draft_hash"]:
                verdict["reason"] = "draft_mismatch"
            else:
                doc.update(used_at=_iso(now), used_by=int(approver), approved=bool(approved))
            return doc

        doc = self.store.update("approvals", nonce, fn)
        if doc is None:
            return None, "unknown"
        return doc, verdict.get("reason")

    def on_owner_decision(self, nonce: str, approver_tg_id: int, *, approved: bool = True,
                          text: str | None = None, new_issue: bool = False) -> dict:
        """Continue the flow after the owner's Telegram tap (a SEPARATE call from the webhook that asked). Locks only
        THIS ticket's sender, so a slow PROVE-IT never blocks another customer."""
        doc0 = self.store.get("approvals", nonce)
        if new_issue and (doc0 or {}).get("kind") != "reopen":
            return {"ok": False, "reason": "new_issue_only_applies_to_reopen"}
        state0 = self.ticket(doc0["ticket_id"]) if doc0 else None
        with self._wa_lock(state0.wa_id if state0 else ""):
            now = self.clock()
            doc, reason = self._consume(nonce, approver_tg_id, approved, now)
            if reason:
                log.warning("owner decision refused: %s (ticket %s)", reason, (doc or {}).get("ticket_id"))
                return self._refused(doc, reason, nonce)
            state = self.ticket(doc["ticket_id"])
            side = doc["kind"] in SIDE_KINDS
            if state is None or (not side and state.pending_nonce != nonce):
                return {"ok": False, "reason": "stale"}
            if not side:
                state.pending_nonce, state.pending_kind = "", ""
            try:
                if new_issue:
                    return self._new_issue(state)
                if not approved:
                    return self._rejected(state, doc)
                return self._apply(state, doc, text, now)
            except Exception as e:                           # noqa: BLE001 - a client send never raises to here
                log.error("owner action failed on ticket %s: %s", state.tid, type(e).__name__)
                self._hold("approval_action_failed", ticket_id=state.tid, detail=type(e).__name__)
                self._safe_ensure(state.tid)                 # re-ask the step the ticket is NOW in
                return {"ok": False, "reason": "action_failed"}

    def _refused(self, doc: dict | None, reason: str, nonce: str) -> dict:
        out = {"ok": False, "reason": reason}
        if reason == "expired" and doc:                       # the (authorised) owner tapped a stale button: re-ask
            state = self.ticket(doc["ticket_id"])
            if state is not None and state.pending_nonce == nonce:
                state.pending_nonce, state.pending_kind = "", ""
                self._save(state)
                self._safe_ensure(state.tid)
                out["reissued"] = self._live_pending(self.ticket(state.tid))
        return out

    def _rejected(self, state: TicketState, doc: dict) -> dict:
        kind = doc["kind"]
        self._note(state.tid, f"[Hub] {kind}: owner declined. Nothing was sent to the client.")
        if kind == "advance":                                 # the reply was not a confirmation: keep waiting
            state.stage = "awaiting_customer"
            self._save(state)
            self._post(state, "Marked NOT confirmed: the ticket stays awaiting the customer.")
        elif kind == "reopen":                                # keep closed: the NEXT message opens a new ticket
            state.keep_closed, state.post_resolve_ids = True, []
            self._save(state)
            self._post(state, "Kept closed. A new message from this customer opens a NEW ticket.")
            self._triage(f"owner kept closed: ticket={state.tid}; the customer's message is archived as a note on it; "
                         "the next message opens a new ticket")
        elif kind in SIDE_KINDS:
            pass                                                # a declined status update changes nothing
        else:
            state.declined = True
            self._save(state)
            if kind == "fix_ready":
                self._post(state, "Fix not ready: use re-ask when it is.")
            if kind in CLIENT_KINDS:
                self._hold("draft_rejected", ticket_id=state.tid, detail=f"{kind} draft declined: send your own text "
                           "(on_owner_text) or re-ask; the customer's next message also re-drafts it")
        return {"ok": True, "sent": False, "stage": state.stage}

    def _new_issue(self, state: TicketState) -> dict:
        """The owner chose NEW ISSUE for the message(s) that raised the "reopen?" question. They are re-fed through the
        NORMAL retryable path (``_handle_one``): a new ticket opens with its own single ack, and a helpdesk/Telegram blip
        gets triage + the tick's backoff retry instead of losing the issue. The old ticket stays resolved."""
        from .model import Inbound
        ids, state.post_resolve_ids, state.keep_closed = list(state.post_resolve_ids), [], True
        self._save(state)
        self._note(state.tid, "[Hub] The owner treated the customer's message as a NEW issue (new ticket).")
        last, failed = None, False
        for mid in ids:
            doc = self.store.get("messages", mid)
            if not doc:
                continue
            self.store.update("messages", mid, lambda d: {**d, "status": "failed", "next_at": "", "attempts": 0})
            if failed:                           # keep the sender's order: later messages wait for the failed one
                continue
            last = self._handle_one(Inbound.from_dict(doc["inbound"]))
            failed = bool(last.get("error"))
        return {"ok": True, "new_issue": True, "ticket_id": (last or {}).get("ticket_id"), "pending_retry": failed}

    def _apply(self, state: TicketState, doc: dict, text: str | None, now: datetime) -> dict:
        kind = doc["kind"]
        if kind in SIDE_KINDS and state.stage == "resolved":
            return {"ok": False, "reason": "stale"}
        if kind in CLIENT_KINDS:
            return self._send_approved(state, doc, text, now)
        if kind == "advance":
            return self._on_advance(state)
        if kind == "fix_ready":
            return self._prove_and_route(state)
        if kind == "reopen":
            return self._on_reopen(state)
        return {"ok": False, "reason": "unknown_kind"}

    # ---------------------------------------------------------------- client-facing sends (owner approved)

    def _refuse_edit(self, state: TicketState, doc: dict, reason: str, redraft: str, why: str) -> dict:
        self._post(state, f"Edited text NOT sent: {why}")
        self._request_approval(state, doc["kind"], redraft)
        return {"ok": False, "reason": reason}

    def _check_edit(self, state: TicketState, doc: dict, final: str) -> dict | None:
        orig = doc["draft"]
        fresh = [f for f in redact.scan(final) if final[f.start:f.end] not in orig]
        if fresh:
            kinds = ", ".join(sorted({f.kind for f in fresh}))
            return self._refuse_edit(state, doc, "edit_contains_sensitive_data", orig, f"it contains {kinds}")
        j = self._judge_draft(state, final)
        if j["flag"]:
            return self._refuse_edit(state, doc, "edited_text_flagged", final,
                                     "the judge flagged it; approve the exact text again to send it")
        return None

    def _send_approved(self, state: TicketState, doc: dict, text: str | None, now: datetime) -> dict:
        kind, draft = doc["kind"], doc["draft"]
        if self._interrupted(state):                         # no approval path may send (or clear the marker)
            return {"ok": False, "reason": "send_outcome_unknown", "stage": state.stage}
        final = draft if text is None else text
        edited = final != draft
        if edited:
            refused = self._check_edit(state, doc, final)
            if refused:
                return refused
        if "[[" in final:
            return self._refuse_edit(state, doc, "placeholder_left", draft, "an unfilled [[placeholder]] remains")
        if not self._window_open(state, now):
            return self._window_closed(state, kind)
        last = min(datetime.fromisoformat(state.last_inbound_at), now)
        state.attempting_kind, state.replies_at_attempt = kind, len(state.replies)
        self._save(state)                                    # persisted BEFORE the send (as the ack does)
        try:
            self.whatsapp.send_text(state.wa_id, final, last_inbound_at=last, now=now, automatic=False)
        except whatsapp_cloud.WindowClosed:
            state.attempting_kind = ""
            return self._window_closed(state, kind)
        except Exception as e:                               # noqa: BLE001 - e.g. a timeout AFTER Meta accepted it
            state.send_unknown, state.attempting_kind = kind, ""
            self._save(state)
            self._hold("send_outcome_unknown", ticket_id=state.tid,
                       detail=f"the {kind} send ended with {type(e).__name__}: check WhatsApp manually; NOT resent")
            return {"ok": False, "reason": "send_unknown", "stage": state.stage}
        state.attempting_kind = ""
        state.human_edits = state.human_edits or edited
        return self._after_send(state, kind, now, edited)

    def _window_closed(self, state: TicketState, kind: str) -> dict:
        # NOTE: outside the 24h window only an APPROVED WhatsApp template may be sent (see WhatsAppCloud.send_template).
        self._save(state)
        self._post(state, f"{kind} approved but NOT sent: the customer's 24h window is closed.")
        self._hold("window_closed", ticket_id=state.tid,
                   detail=f"{kind} approved but the 24h window is closed: needs an approved template; the customer's "
                          "next message re-asks you")
        return {"ok": False, "reason": "window_closed", "stage": state.stage}

    def _after_send(self, state: TicketState, kind: str, now: datetime, edited: bool) -> dict:
        """The message is OUT: persist the new stage FIRST, then do the best-effort follow-ups (never re-sends)."""
        if kind == "confirm":
            state.stage = "awaiting_customer"
            state.history.append("8-CONFIRM")
        elif kind == "reply":
            state.stage, state.needs_owner_text = "awaiting_customer", False
            state.history.append("OWNER-REPLY")
        elif kind == "status":                                # a side message: stage and live approval untouched
            state.history.append("OWNER-STATUS")
        else:
            state.needs_owner_text = False
            self._expire_side(state)
            state.stage = "resolved"
            state.resolved_at = _iso(now)
            state.history.append("10-RESOLVE")
        self._save(state)
        self._note(state.tid, f"[Hub] {kind} reply sent to client (owner-approved{', edited' if edited else ''}).")
        if kind == "resolve":
            self._close_out(state)
        return {"ok": True, "sent": True, "stage": state.stage, "edited": edited}

    def _close_out(self, state: TicketState) -> None:
        link = state.proof.get("share_link")
        if link:
            self._note(state.tid, f"[Hub] Proof video attached (share link): {html.escape(str(link))}")
        a = state.analysis
        pj = state.proof_judge or {}
        outcome = {"ticket_id": state.tid, "client": state.client, "type": a.ticket_type, "module": a.module,
                   "owner_uid": state.owner.uid if state.owner else 0, "resolved": True,
                   "proof_outcome": state.proof.get("verdict"), "judge_video_score": pj.get("aggregate"),
                   "judge_flag": pj.get("flag"), "reopen_count": state.reopen_count}
        try:
            kpi = self.kpi.push_kpis(outcome)
            state.history.append("11-KPIS")
            self._save(state)
            if not kpi.get("ok"):
                self._hold("kpi_push_blocked", ticket_id=state.tid, detail=str(kpi.get("why")))
            self.learning.record({
                "ticket_id": state.tid, "client": state.client, "partner_id": state.partner_id,
                "final_type": a.ticket_type, "final_module": a.module,
                "final_owner_uid": state.owner.uid if state.owner else 0,
                "final_owner_name": state.owner.name if state.owner else "", "reassigned": state.reassigned,
                "resolution": f"{a.module}: {a.summary}", "human_edits": state.human_edits, "client_rating": None,
                "proof_outcome": state.proof.get("verdict"), "judge_scores": {"video": pj.get("aggregate")},
                "ts": _iso(self.clock())})
        except Exception as e:                               # noqa: BLE001
            log.error("close-out failed on ticket %s: %s", state.tid, type(e).__name__)
            self._hold("closeout_failed", ticket_id=state.tid, detail=type(e).__name__)

    # ---------------------------------------------------------------- owner commands (actor-gated)

    def on_owner_text(self, tid: int, actor_tg_id, text: str, *, kind: str = "reply") -> dict:
        """The owner WRITES the client message (decline, inquiry/guidance answer, status update): it becomes an
        owner-authored draft that takes the NORMAL path: scan -> judge -> bound approval -> send (24h window checked).
        Nobody replies outside the hub. ``kind``: confirm | resolve | reply (answers a guidance/inquiry ticket) |
        status (a side message: no stage change, the ticket's own approval stays live; ``reply`` on a ticket that does
        not wait for an owner answer is treated as ``status``)."""
        state = self.ticket(int(tid))
        if state is None or kind not in CLIENT_KINDS:
            return {"ok": False, "reason": "unknown_ticket_or_kind"}
        with self._wa_lock(state.wa_id):
            state = self.ticket(int(tid))
            if not self._actor_ok(actor_tg_id, state):
                return {"ok": False, "reason": "not_authorised"}
            text = str(text or "").strip()
            if not text or "[[" in text:
                return {"ok": False, "reason": "empty_or_placeholder"}
            if state.stage == "resolved":
                return {"ok": False, "reason": "ticket_closed",
                        "message": "the ticket is resolved: use reopen (or new-issue) on the customer's message first"}
            if self._interrupted(state):
                return {"ok": False, "reason": "send_outcome_unknown"}
            if redact.scan(text):
                return {"ok": False, "reason": "edit_contains_sensitive_data"}
            if kind == "reply" and not state.needs_owner_text:
                kind = "status"
            self._request_approval(state, kind, text)
            return {"ok": True, "pending": kind}

    def close_ticket(self, tid: int, actor_tg_id, *, note: str = "") -> dict:
        """Owner closes a ticket WITHOUT a client message (e.g. after an owner-authored reply): KPI + learning run."""
        state = self.ticket(int(tid))
        if state is None:
            return {"ok": False, "reason": "unknown_ticket"}
        with self._wa_lock(state.wa_id):
            state = self.ticket(int(tid))
            if not self._actor_ok(actor_tg_id, state):
                return {"ok": False, "reason": "not_authorised"}
            if state.stage == "resolved":
                return {"ok": True, "stage": "resolved", "already": True}
            if state.pending_nonce:
                self.store.update("approvals", state.pending_nonce,
                                  lambda d: d if d.get("used_at") else {**d, "used_at": "superseded"})
            state.pending_nonce, state.pending_kind, state.needs_owner_text = "", "", False
            self._expire_side(state)
            state.stage, state.resolved_at = "resolved", _iso(self.clock())
            state.history.append("10-RESOLVE(owner-closed)")
            self._save(state)
            self._close_holds(ticket_id=state.tid)
            self._note(state.tid, f"[Hub] Closed by the owner without a client message. {html.escape(note)}".strip())
            self._close_out(state)
            return {"ok": True, "stage": "resolved"}

    def reask(self, tid: int, actor_tg_id) -> dict:
        """Owner command: ask for the current step again (after an expiry, a decline or a Telegram failure)."""
        state = self.ticket(int(tid))
        if state is None:
            return {"ok": False, "reason": "unknown_ticket"}
        with self._wa_lock(state.wa_id):
            state = self.ticket(int(tid))
            if not self._actor_ok(actor_tg_id, state):
                return {"ok": False, "reason": "not_authorised"}
            state.declined = False
            asked = self._ensure_pending(state, force=True)
            return {"ok": True, "asked": asked}

    def confirm_unknown_send(self, tid: int, actor_tg_id, *, sent: bool) -> dict:
        """The owner checked WhatsApp after an UNKNOWN send result: ``sent=True`` -> proceed as sent; False -> the
        draft is re-asked. The hub never decides this by itself, so a message can never go out twice."""
        state = self.ticket(int(tid))
        if state is None:
            return {"ok": False, "reason": "unknown_ticket"}
        with self._wa_lock(state.wa_id):
            state = self.ticket(int(tid))
            if not self._actor_ok(actor_tg_id, state):
                return {"ok": False, "reason": "not_authorised"}
            kind = state.send_unknown
            if not kind:
                return {"ok": False, "reason": "nothing_unknown"}
            state.send_unknown = ""
            self._close_holds(ticket_id=state.tid, reason="send_outcome_unknown")
            if sent:
                late = len(state.replies) > state.replies_at_attempt and kind in ("confirm", "reply")
                out = self._after_send(state, kind, self.clock(), False)
                if late:                         # the customer answered while the outcome was unknown: re-offer it
                    state = self.ticket(state.tid)
                    state.stage = "awaiting_owner_advance"
                    self._request_approval(state, "advance", state.replies[-1]["text"], judged=False)
                    out["stage"] = state.stage
                return out
            self._save(state)
            self._ensure_pending(state, force=True)
            return {"ok": True, "stage": state.stage}

    # ---------------------------------------------------------------- owner-marked confirm, fix, PROVE-IT, RESOLVE

    def _on_advance(self, state: TicketState) -> dict:
        state.stage = "in_progress"
        state.history.append("8b-CONFIRMED")
        self._save(state)
        self._post(state, "Owner marked the customer's reply as the confirmation. Work starts.")
        self._note(state.tid, "[Hub] Owner confirmed the customer's reply; work starts.")
        if state.analysis.ticket_type == "Inquiries":
            self._guidance_hold(state, "inquiry: no proof video; write the answer with on_owner_text, then close_ticket")
        else:
            self._request_approval(state, "fix_ready",
                                   f"Fix ready for ticket #{state.tid}? Approve to run PROVE-IT.", judged=False)
        return {"ok": True, "sent": False, "stage": state.stage}

    def _on_reopen(self, state: TicketState) -> dict:
        """The owner chose REOPEN after a customer message on a resolved ticket."""
        state.reopen_count += 1
        state.stage, state.resolved_at, state.keep_closed = "in_progress", "", False
        state.needs_owner_text, state.post_resolve_ids = False, []
        self._expire_side(state)
        state.proof, state.proof_judge, state.resolve_mode = {}, {}, ""
        state.history.append(f"REOPENED#{state.reopen_count}")
        self._save(state)
        self._note(state.tid, "[Hub] Owner reopened the ticket after a customer message.")
        self._post(state, "REOPENED by the owner. Work resumes.")
        if state.analysis.ticket_type == "Inquiries":
            self._guidance_hold(state, "reopened inquiry: write the answer with on_owner_text, then close_ticket")
        else:
            self._request_approval(state, "fix_ready",
                                   f"Fix ready for ticket #{state.tid}? Approve to run PROVE-IT.", judged=False)
        return {"ok": True, "sent": False, "stage": state.stage}

    def _guidance_hold(self, state: TicketState, why: str) -> None:
        # There is no guidance / not-reproduced client template, so the hub drafts NOTHING here;
        # the owner writes the text (on_owner_text) and it takes the normal judge -> approval -> send path.
        state.needs_owner_text = True
        self._save(state)
        self._post(state, f"Guidance needed ({why}). No client draft was generated.")
        self._hold("guidance_needed", ticket_id=state.tid, detail=why)

    def _prove_and_route(self, state: TicketState) -> dict:
        """PROVE-IT runs ONCE per owner "fix ready" tap; then RESOLVE is routed by the verdict."""
        a = state.analysis
        request = {"id": f"REQ-{state.tid}", "company": state.client, "ticket_ref": str(state.tid)}
        packet = {"request_id": f"REQ-{state.tid}", "company": state.client, "tier": "T2b", "understanding": a.summary}
        proof = self.proof.prove(request, packet, self.cfg)
        state.proof = proof
        # the judge sees status/verdict only: the share link carries the client's name
        jv = self._judge("video", {k: proof.get(k) for k in ("status", "verdict", "route")})
        state.proof_judge = {"aggregate": jv["aggregate"], "flag": jv["flag"]}
        if proof.get("status") != "prepared" or not proof.get("share_link"):
            state.history.append("9-PROVE-IT(blocked)")
            self._hold("proof_blocked", ticket_id=state.tid, detail=str(proof.get("why") or proof.get("blockers")))
            self._request_approval(state, "fix_ready", f"Retry PROVE-IT for ticket #{state.tid}?", judged=False)
            return {"ok": True, "sent": False, "stage": state.stage, "blocked": "proof not ready"}
        if proof.get("verdict") not in RESOLUTION_VERDICTS:
            state.history.append("9-PROVE-IT(guidance)")
            self._guidance_hold(state, f"proof verdict {proof.get('verdict')}: write the reply with on_owner_text")
            return {"ok": True, "sent": False, "stage": state.stage}
        state.history.append("9-PROVE-IT")
        state.stage = "awaiting_owner_resolve"
        self._request_approval(state, "resolve", self._resolve_draft(state))
        return {"ok": True, "sent": False, "stage": state.stage}
