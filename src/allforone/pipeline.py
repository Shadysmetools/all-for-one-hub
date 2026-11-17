# SPDX-License-Identifier: Apache-2.0
"""The Hub: the support pipeline orchestrator.

  1 RECEIVE -> 2 STT -> 3 ANALYSE -> 4 CREATE -> 5 ASSIGN -> 6 TELEGRAM -> 7 ACK -> 8 CONFIRM (owner approval, async)
  -> customer reply (attached, never auto-advances) -> owner marks confirmed -> fix -> owner taps fix-ready
  -> 9 PROVE-IT -> 10 RESOLVE (owner approval, async) -> 11 KPIs -> (close -> learning)

The owner side (approvals, resolve routing, holds) lives in ``owner_flow.py``. State is per TICKET and durable
(``store.py``); a message is marked DONE only after it was processed, a failed one stays retryable, and the first
contact saves the ticket right after CREATE so a retry resumes instead of creating an orphan twin.

Gates and safety:
  - The ACK is the ONLY automatic client message, at most one per ticket. CONFIRM and RESOLVE are owner-approved
    first (bound approvals), and never sent outside the 24h window (checked against the real now).
  - The judge scores each client-reply DRAFT before the owner sees it; a blocked/failed judge means UNJUDGED.
  - Helpdesk writes stay inside ``helpdesk/guard.py`` with mail suppressed on create (``GuardedHelpdesk``).
  - An unknown sender (no partner) is HELD for a human (triage topic + hold queue): no ticket, no message, and
    no audio sent to STT.
  - A PER-SENDER lock serialises everything that touches one customer's tickets (webhooks, owner taps, PROVE-IT), so
    a text + voice note a second apart make ONE ticket while other customers are never blocked.

DEPLOY CONSTRAINT: ONE hub process per SQLite file. The locks are in-process; a second process (a separate
scheduler, a blue/green twin) could race the check-then-create and send two acks. ``SqliteStore.lock_exclusive``
refuses a second hub on the same file. Run ``python -m allforone.tick`` only as a diagnostic.
"""
from __future__ import annotations

import html
import logging
import threading
from datetime import UTC, datetime, timedelta

from . import analyse as analyse_mod
from .channels import whatsapp_cloud
from .fakes import merge_context
from .helpdesk.gateway import GuardedHelpdesk, mail_suppress_context, team_id_for, x_track_value, x_type_id
from .model import Analysis, Inbound, Owner, TicketState
from .owner_flow import OwnerFlow
from .store import MemoryStore
from .templates import Templates

log = logging.getLogger("allforone")

OPEN_STAGES = {"new", "awaiting_owner", "awaiting_customer", "awaiting_owner_advance", "in_progress",
               "awaiting_owner_resolve"}


def _html(analysis: Analysis, inbound: Inbound) -> str:
    e = html.escape
    qs = "".join(f"<li>{e(q)}</li>" for q in analysis.open_questions)
    qblock = f"<p><b>Open questions:</b></p><ul>{qs}</ul>" if qs else ""
    return (f"<p><b>Source:</b> WhatsApp ({e(inbound.kind)}) from {e(inbound.name or inbound.wa_id)}</p>"
            f"<p><b>Request:</b> {e(analysis.summary)}</p>"
            f"<p><b>Classification:</b> {e(analysis.nature)} · {e(analysis.ticket_type)} · track {e(analysis.track)} · "
            f"module {e(analysis.module)} (confidence {analysis.confidence})</p>"
            f"<p><b>Likely root cause / scope:</b> {e(analysis.root_cause)}; {e(analysis.scope)}</p>{qblock}")


class Hub(OwnerFlow):
    """One hub instance owns the pipeline across webhooks. Inject every seam; the fakes make it CI-safe."""

    def __init__(self, *, helpdesk: GuardedHelpdesk, whatsapp, stt, telegram, knowledge, knowledge_notes,
                 assigner, proof, kpi_sink, judge, learning, cfg: dict | None = None, store=None,
                 clock=None):
        self.helpdesk = helpdesk
        self.whatsapp = whatsapp
        self.stt = stt
        self.telegram = telegram
        self.k_primary = knowledge
        self.k_notes = knowledge_notes
        self.assigner = assigner
        self.proof = proof
        self.kpi = kpi_sink
        self.judge = judge
        self.learning = learning
        self.cfg = cfg or {}
        self.conv = helpdesk.conv
        if store is None and self.cfg.get("run_mode") not in ("test", "dry_run"):
            raise ValueError("Hub needs an explicit store (SqliteStore); MemoryStore only when run_mode is test/dry_run")
        self.store = store if store is not None else MemoryStore()
        self.clock = clock or (lambda: datetime.now(UTC))
        lock = getattr(self.store, "lock_exclusive", None)
        if lock:
            lock()                                   # one hub process per SQLite file (refuses a second one)
        self._locks: dict[str, threading.RLock] = {}
        self._locks_guard = threading.Lock()
        safe = set(self.conv.safe_stages or set())
        self.tpl = Templates.from_config(self.cfg)
        self.modules = [(n, tuple(k)) for n, k in (self.cfg.get("modules") or {}).items()] or None
        want = int((self.cfg.get("helpdesk") or {}).get("working_stage") or 0)
        self.working_stage = want if want in safe else (sorted(safe)[0] if safe else want)

    def _wa_lock(self, wa_id: str) -> threading.RLock:
        with self._locks_guard:
            return self._locks.setdefault(str(wa_id), threading.RLock())

    # ---------------------------------------------------------------- ticket access

    def _save(self, state: TicketState) -> None:
        self.store.put("tickets", state.tid, state.to_dict())

    def ticket(self, tid: int) -> TicketState | None:
        d = self.store.get("tickets", tid)
        return TicketState.from_dict(d) if d else None

    def all_tickets(self) -> list[TicketState]:
        return [TicketState.from_dict(d) for d in self.store.scan("tickets")]

    def tickets_for(self, wa_id: str) -> list[TicketState]:
        return [t for t in self.all_tickets() if t.wa_id == str(wa_id)]

    def latest_ticket(self, wa_id: str) -> TicketState | None:
        mine = self.tickets_for(wa_id)
        return mine[-1] if mine else None

    def _ticket_by_source(self, message_id: str) -> TicketState | None:
        for t in self.all_tickets():
            if t.source_message_id == message_id:
                return t
        return None

    # ---------------------------------------------------------------- entry

    def handle_webhook(self, payload: dict) -> dict:
        from .webhook import parse_all
        inbounds = parse_all(payload, phone_number_id=str(self.cfg.get("phone_number_id") or ""))
        if not inbounds:
            return {"ignored": "no inbound message for this hub number (status callback, other number or "
                               "unsupported type)"}
        results = [self._handle_one(i) for i in inbounds]
        return results[0] if len(results) == 1 else {"batch": results}

    def accept(self, payload: dict) -> int:
        """Persist every inbound message as ``queued`` BEFORE the webhook answers 200 (Meta won't resend after a 200),
        so a crash before processing loses nothing: ``retry_failed`` drains what is left. Returns how many NEW."""
        from .webhook import parse_all
        n = 0                           # NO hub lock: put_if_absent is atomic, and the lock is held across slow STT/PROVE-IT
        for i in parse_all(payload, phone_number_id=str(self.cfg.get("phone_number_id") or "")):
            i.message_id = i.message_id or f"nomid-{id(i)}-{self.clock().timestamp()}"
            n += bool(self.store.put_if_absent("messages", i.message_id, {
                "status": "queued", "since": self.clock().isoformat(), "attempts": 0, "inbound": i.to_dict(),
                "error": ""}))
        return n

    BACKOFF_S = (30, 120, 600, 3600, 21600)     # 30s, 2m, 10m, 1h, 6h, then the message goes DEAD (+ triage)

    def _handle_one(self, inbound: Inbound) -> dict:
        """Claim -> route -> mark. A message is DONE only after it was processed; an exception leaves it FAILED with a
        backoff (``tick`` re-drives it), tells a human once, and never loses it."""
        mid = inbound.message_id or f"nomid-{id(inbound)}-{self.clock().timestamp()}"
        inbound.message_id = mid
        with self._wa_lock(inbound.wa_id):
            if not self._claim(inbound):
                return {"duplicate": True, "message_id": mid}      # idempotency (A2)
            try:
                res = self._route(inbound)
            except Exception as e:                                 # noqa: BLE001
                log.error("message %s failed: %s", mid, type(e).__name__)
                self._failed(mid, type(e).__name__)
                return {"error": type(e).__name__, "retryable": True, "message_id": mid}
            self._finish(mid, "held" if res.get("held") else "done")
            self._close_holds(message_id=mid, reason="processing_failed")
            return res

    def _failed(self, mid: str, err: str) -> None:
        now = self.clock()
        doc = self.store.get("messages", mid) or {}
        n = int(doc.get("attempts", 1))
        src = self._ticket_by_source(mid)
        tid = src.tid if src else 0
        if n > len(self.BACKOFF_S):
            self._finish(mid, "dead", err)
            self._hold("processing_dead", ticket_id=tid, message_id=mid,
                       detail=f"message dead after {n} attempts ({err}); needs a human")
            return
        self._finish(mid, "failed", err, next_at=(now + timedelta(seconds=self.BACKOFF_S[n - 1])).isoformat())
        if n == 1 or not any(h["reason"] == "processing_failed" and h.get("message_id") == mid for h in self.holds()):
            self._hold("processing_failed", ticket_id=tid, message_id=mid,
                       detail=f"processing error {err}; automatic retry is scheduled")

    def _claim(self, inbound: Inbound) -> bool:
        now = self.clock()
        doc = {"status": "processing", "since": now.isoformat(), "attempts": 1, "inbound": inbound.to_dict(),
               "error": ""}
        if self.store.put_if_absent("messages", inbound.message_id, doc):
            return True
        took: list[int] = []
        timeout = float(self.cfg.get("processing_timeout_s", 300))

        def fn(d: dict) -> dict:
            stale = d["status"] == "processing" and now - datetime.fromisoformat(d["since"]) > timedelta(seconds=timeout)
            if d["status"] in ("failed", "queued") or stale:
                d.update(status="processing", since=now.isoformat(), attempts=d["attempts"] + 1)
                took.append(1)
            return d

        self.store.update("messages", inbound.message_id, fn)
        return bool(took)

    def _finish(self, mid: str, status: str, error: str = "", next_at: str = "") -> None:
        self.store.update("messages", mid, lambda d: {**d, "status": status, "error": error, "next_at": next_at})

    def retry_failed(self, *, due_only: bool = False, now: datetime | None = None) -> list[dict]:
        """Re-drive every FAILED / queued / stale in-flight message once (``dead`` ones are left for a human).
        ``due_only`` honours each message's backoff (``tick``); a manual call retries immediately."""
        now = now or self.clock()
        out, blocked = [], set()
        for doc in self.store.scan("messages"):
            if doc.get("status") not in ("failed", "queued", "processing"):
                continue
            if due_only and doc.get("next_at") and datetime.fromisoformat(doc["next_at"]) > now:
                blocked.add(doc["inbound"]["wa_id"])          # an earlier message of this sender is still waiting
                continue
            if doc["inbound"]["wa_id"] in blocked:            # keep each sender's order: later ones wait for it
                continue
            res = self._handle_one(Inbound.from_dict(doc["inbound"]))
            if res.get("error"):
                blocked.add(doc["inbound"]["wa_id"])
            out.append(res)
        return [r for r in out if not r.get("duplicate")]

    def tick(self, now: datetime | None = None) -> dict:
        """The scheduler calls this every minute: retry due messages (backoff), expire stale approvals (+ triage),
        and re-ask INTERNAL owner steps a ticket lost (e.g. Telegram was down). Client-facing drafts are never
        re-asked on a timer: the owner (``reask``) or the customer's next message does that."""
        retried = len(self.retry_failed(due_only=True, now=now))
        expired = reasked = 0
        for t in self.all_tickets():
            if t.stage in ("resolved", "new", "awaiting_customer"):
                continue
            with self._wa_lock(t.wa_id):
                st = self.ticket(t.tid)
                if st is None:
                    continue
                if st.pending_nonce and not self._live_pending(st):
                    doc = self.store.get("approvals", st.pending_nonce) or {}
                    if not doc.get("used_at"):
                        self.store.update("approvals", st.pending_nonce, lambda d: {**d, "used_at": "expired"})
                        self._triage(f"approval expired: ticket={st.tid} kind={st.pending_kind}: use re-ask")
                        expired += 1
                    st.pending_nonce, st.pending_kind = "", ""
                    self._save(st)
                try:
                    reasked += bool(self._ensure_pending(st, internal_only=True))
                except Exception as e:                             # noqa: BLE001
                    log.error("tick re-ask failed on ticket %s: %s", st.tid, type(e).__name__)
        return {"retried": retried, "expired": expired, "reasked": reasked}

    # ---------------------------------------------------------------- routing: per TICKET, not per phone

    def _route(self, inbound: Inbound) -> dict:
        resume = self._ticket_by_source(inbound.message_id)
        if resume is not None:       # reprocessing the message that OPENED a ticket == its first contact failed midway
            return self._continue_first_contact(resume, [])      # (idempotent: every stage is guarded by history)
        state = self.latest_ticket(inbound.wa_id)
        if state is None:
            return self._first_contact(inbound)
        if state.stage == "resolved":
            if state.keep_closed or not self._within_reopen(state):
                return self._first_contact(inbound)                 # kept closed / too late for a reply: a NEW ticket
            return self._post_resolve(state, inbound)
        return self._attach(state, inbound)

    def _within_reopen(self, state: TicketState) -> bool:
        if not state.resolved_at:
            return False
        age = self.clock() - datetime.fromisoformat(state.resolved_at)
        return age <= timedelta(days=float(self.cfg.get("reopen_days", 7)))

    # ---------------------------------------------------------------- stages 1-8 (first contact)

    def _transcribe(self, inbound: Inbound, log_: list) -> bool:
        if inbound.kind == "voice":
            tr = self.stt.transcribe(inbound.audio_id or "", lang_hint=inbound.lang)
            if tr.get("status") == "blocked" or not tr.get("text"):
                log_.append({"stage": "2-STT", "status": "blocked", "why": tr.get("why")})
                return False
            inbound.text = tr["text"]
            inbound.lang = tr.get("lang") or inbound.lang
            inbound.transcript_placeholder = bool(tr.get("placeholder"))
            log_.append({"stage": "2-STT", "status": "ok", "provider": tr.get("provider"),
                         "placeholder": inbound.transcript_placeholder, "audio_retained": tr.get("audio_retained")})
        else:
            inbound.resolve_lang()
            log_.append({"stage": "2-STT", "status": "skipped", "reason": f"{inbound.kind} message"})
        return True

    def _ctx(self, partner_id: int) -> dict:
        return merge_context(self.k_primary.client_context(partner_id), self.k_notes.client_context(partner_id))

    def _first_contact(self, inbound: Inbound) -> dict:
        log_: list[dict] = [{"stage": "1-RECEIVE", "status": "ok", "kind": inbound.kind,
                             "message_id": inbound.message_id}]
        # identity FIRST (A4 + data minimisation): a stranger's voice note must never reach the STT provider
        # NOTE: a KNOWN sender's voice note does go to the STT provider; the proof share_link carries the client
        # name (kept away from the judge). Decide your own audio-retention policy.
        partner_id = self.k_primary.partner_for(inbound.wa_id)
        if not partner_id:
            return self._hold("unknown_sender", inbound=inbound, log_=log_,
                              detail="no res.partner for this sender; no ticket, no message, no STT")
        if not self._transcribe(inbound, log_):
            return self._hold("stt_unavailable", inbound=inbound, log_=log_, detail="transcription unavailable")
        if not (inbound.text or "").strip():
            return self._hold("empty_message", inbound=inbound, log_=log_, detail="no text to open a ticket from")

        ctx = self._ctx(partner_id)
        client = str(ctx.get("name") or ctx.get("client") or f"partner:{partner_id}")
        analysis = analyse_mod.analyse(inbound.text or "", lang=inbound.lang, client_context=ctx,
                                       modules=self.modules)
        jcls = self._judge("classification", {"ticket_type": analysis.ticket_type, "module": analysis.module,
                                              "confidence": analysis.confidence})
        log_.append({"stage": "3-ANALYSE", "status": "ok", "nature": analysis.nature,
                     "ticket_type": analysis.ticket_type, "track": analysis.track, "module": analysis.module,
                     "judge": jcls["aggregate"]})

        tid = self._create(analysis, inbound, int(partner_id))
        log_.append({"stage": "4-CREATE", "status": "ok", "ticket_id": tid, "mail_suppressed": True})
        state = TicketState(wa_id=inbound.wa_id, inbound=inbound, analysis=analysis, partner_id=int(partner_id),
                            tid=tid, lang=inbound.lang, client=client, source_message_id=inbound.message_id,
                            last_inbound_at=inbound.ts.isoformat())
        state.history = ["1-RECEIVE", "2-STT", "3-ANALYSE", "4-CREATE"]
        self._save(state)                                           # right after CREATE: no orphan on a later failure
        return self._continue_first_contact(state, log_)

    def _create(self, analysis: Analysis, inbound: Inbound, partner_id: int) -> int:
        vals = {"name": f"[WA] {analysis.ticket_type}: {analysis.summary[:50]}",
                "description": _html(analysis, inbound), "team_id": team_id_for(analysis.track, self.conv, (self.cfg.get("helpdesk") or {}).get("teams")),
                "stage_id": self.working_stage, "partner_id": partner_id, "priority": "1"}
        if self.conv.has("x_track"):
            vals["x_track"] = x_track_value(analysis.track, self.conv)
        tid_type = x_type_id(analysis.ticket_type, self.conv)
        if self.conv.has("x_type") and tid_type:
            vals["x_type"] = tid_type
        if self.conv.has("x_ai_confidence"):
            vals["x_ai_confidence"] = analysis.confidence
        return self.helpdesk.create_ticket(vals, context=mail_suppress_context())

    def _continue_first_contact(self, state: TicketState, log_: list) -> dict:
        """Stages 5-8, each guarded by ``history`` so a retry resumes exactly where the failure happened."""
        if "5-ASSIGN" not in state.history:
            choice = self.assigner.assign(state.analysis, self._ctx(state.partner_id))
            state.owner = choice["owner"]
            state.rationale = choice["rationale"]
            self.helpdesk.assign(state.tid, state.owner.uid)
            state.history.append("5-ASSIGN")
            self._save(state)
            log_.append({"stage": "5-ASSIGN", "status": "ok", "owner": state.owner.name, "uid": state.owner.uid})
        if not state.topic_id:
            brief = self._brief(state)
            state.topic_id = self.telegram.open_topic(state.tid, state.owner, brief)
            self.telegram.post(state.topic_id, brief)
            state.history.append("6-TELEGRAM")
            self._save(state)
            log_.append({"stage": "6-TELEGRAM", "status": "ok", "topic_id": state.topic_id})
        self._ack(state, log_)
        if "8-CONFIRM(drafted)" not in state.history:
            draft = self._confirm_draft(state)
            state.history.append("8-CONFIRM(drafted)")
            state.stage = "awaiting_owner"
            self._request_approval(state, "confirm", draft)          # ASYNC: returns pending; the owner's tap resumes
            log_.append({"stage": "8-CONFIRM", "status": "awaiting_owner", "sent": False})
        return {"ticket_id": state.tid, "owner": state.owner.name, "stage": state.stage, "log": log_,
                "awaiting": "owner approval of the CONFIRM draft (nothing sent to the customer yet)"}

    def _ack(self, state: TicketState, log_: list) -> None:
        """Stage 7 — the ONLY automatic client message, at most once per ticket and never outside the window."""
        # A time-boxed FALLBACK ack (templated, no ticket number, when STT/helpdesk/Telegram are slow) is deliberately
        # NOT built; the ack here waits for CREATE/ASSIGN/TELEGRAM.
        h = state.history
        if "7-ACK(attempting)" in h:                                # a send was started and its outcome is unknown
            h[h.index("7-ACK(attempting)")] = "7-ACK(unknown)"
            self._post(state, "Ack send outcome unknown (an earlier attempt failed); NOT retried, so the client "
                              "cannot get two acks. Check WhatsApp manually.")
            self._save(state)
            return
        if any(x in h for x in ("7-ACK", "7-ACK(skipped)", "7-ACK(unknown)")):
            return
        now = self.clock()
        if not self._window_open(state, now):
            h.append("7-ACK(skipped)")
            self._post(state, "Ack skipped: the customer's 24h window is already closed.")
            self._save(state)
            log_.append({"stage": "7-ACK", "status": "skipped", "why": "window closed"})
            return
        h.append("7-ACK(attempting)")
        self._save(state)                                           # persisted BEFORE the send
        a = state.analysis
        text = self.tpl.ack(state.lang, name=state.inbound.name or "there", n=state.tid, owner=state.owner.name,
                             next_step=self.tpl.next_step(state.lang, has_questions=bool(a.open_questions)))
        last = min(datetime.fromisoformat(state.last_inbound_at), now)
        try:
            self.whatsapp.send_text(state.wa_id, text, last_inbound_at=last, now=now, automatic=True)
        except whatsapp_cloud.WindowClosed:
            h[h.index("7-ACK(attempting)")] = "7-ACK(skipped)"
            self._save(state)
            return
        h[h.index("7-ACK(attempting)")] = "7-ACK"
        self._save(state)
        self._note(state.tid, "[Hub] Auto-ack sent to client (Template 1).")
        log_.append({"stage": "7-ACK", "status": "sent", "automatic": True})

    def _brief(self, state: TicketState) -> str:
        ctx = self._ctx(state.partner_id)
        a = state.analysis
        agreed = ", ".join(ctx.get("agreed") or []) or "—"
        commits = ", ".join(ctx.get("open_commitments") or []) or "—"
        collabs = ", ".join(ctx.get("usual_collaborators") or []) or "—"
        return (f"Ticket #{state.tid} → @{state.owner.name}\n"
                f"{state.rationale}\n"
                f"Request: {a.summary}\n"
                f"Type {a.ticket_type} · track {a.track} · module {a.module}\n"
                f"Notes: agreed[{agreed}] open[{commits}] usually-with[{collabs}]\n"
                f"Open questions: {'; '.join(a.open_questions) or 'none'}")

    # ---------------------------------------------------------------- customer messages on an existing ticket

    def _reply_text(self, state: TicketState, inbound: Inbound) -> tuple[str, bool]:
        """(text, ok). ``ok`` False = a voice note that could not be transcribed yet (held for a human)."""
        if inbound.kind == "voice":
            tr = self.stt.transcribe(inbound.audio_id or "", lang_hint=inbound.lang)
            if tr.get("status") == "blocked" or not tr.get("text"):
                if not any(h.get("message_id") == inbound.message_id for h in self.holds()):
                    self._hold("stt_unavailable", inbound=inbound, ticket_id=state.tid,
                               detail="voice reply could not be transcribed")
                return "(voice note: transcription unavailable)", False
            return str(tr["text"]), True
        if inbound.kind == "media":
            cap = f" {inbound.text}" if inbound.text else ""
            return f"[{inbound.media_type} {inbound.media_id}]{cap}", True
        return inbound.text or "", True

    def _record_reply(self, state: TicketState, inbound: Inbound) -> tuple[dict, str]:
        """Store the customer's message on the ticket. Returns (reply, "new" | "updated" | "dup"); a reply whose STT
        was down is RE-TRANSCRIBED when the message is re-driven (hold released)."""
        for r in state.replies:
            if r["message_id"] == inbound.message_id:
                if r.get("pending_stt"):
                    text, ok = self._reply_text(state, inbound)
                    if ok:
                        r["text"] = text
                        r.pop("pending_stt")
                        return r, "updated"
                return r, "dup"
        text, ok = self._reply_text(state, inbound)
        r = {"message_id": inbound.message_id, "kind": inbound.kind, "text": text, "ts": inbound.ts.isoformat()}
        if not ok:
            r["pending_stt"] = True
        state.replies.append(r)
        state.last_inbound_at = max(state.last_inbound_at, inbound.ts.isoformat())
        return r, "new"

    def _attach(self, state: TicketState, inbound: Inbound) -> dict:
        """A message on an OPEN ticket is never dropped and never reprocessed as a new request: it is stored on the
        ticket, mirrored as a helpdesk note and a topic post. Nothing moves forward by itself: a reply while we wait for
        the customer (or a newer one) asks the OWNER to mark it confirmed (an "advance" approval, superseding a stale
        one), and any other stage re-asks the owner if it has lost its approval (expiry, decline, window closed)."""
        r, status = self._record_reply(state, inbound)
        if status == "dup":                       # a RETRY of a reply whose re-ask failed must still re-ask
            self._ensure_pending(state, force=True)
            return {"ticket_id": state.tid, "stage": state.stage, "attached": True}
        label = inbound.media_type if inbound.kind == "media" else inbound.kind
        self._note(state.tid, f"[Hub] Customer reply ({label}): {html.escape(r['text'])}")
        self._post(state, f"Customer reply ({label}): {r['text']}")
        if state.stage in ("awaiting_customer", "awaiting_owner_advance"):
            state.stage = "awaiting_owner_advance"
            self._request_approval(state, "advance", r["text"], judged=False)
        else:
            self._save(state)
            self._ensure_pending(state, force=True)
        return {"ticket_id": state.tid, "stage": state.stage, "attached": True}

    def _post_resolve(self, state: TicketState, inbound: Inbound) -> dict:
        """A message on a RESOLVED ticket within ``reopen_days``: attach it and ask the OWNER "reopen or keep closed?".
        The ticket stays resolved until the owner chooses; more messages join the same open question."""
        r, status = self._record_reply(state, inbound)
        if status == "dup":
            return {"ticket_id": state.tid, "stage": state.stage, "attached": True}
        if inbound.message_id not in state.post_resolve_ids:
            state.post_resolve_ids.append(inbound.message_id)
        self._note(state.tid, f"[Hub] Customer message after resolve: {html.escape(r['text'])}")
        self._post(state, f"Customer wrote after the ticket was resolved: {r['text']}\nReopen or keep closed?")
        if self._live_pending(state) and state.pending_kind == "reopen":
            self._save(state)
        else:
            self._request_approval(state, "reopen", r["text"], judged=False)
        return {"ticket_id": state.tid, "stage": state.stage, "attached": True, "reopen_asked": True}

    # ---------------------------------------------------------------- owner override

    def reassign(self, ref, new_owner: Owner, *, actor_tg_id=None) -> dict:
        """One-tap owner override: write the reassignment and a learning SIGNAL. ``ref`` is a ticket id (int) or a
        sender id (str, latest ticket). A pending approval is re-issued to the NEW owner."""
        state = self.ticket(int(ref)) if isinstance(ref, int) else self.latest_ticket(str(ref))
        if state is None:
            raise KeyError("no such ticket")
        with self._wa_lock(state.wa_id):
            state = self.ticket(state.tid)
            if not self._actor_ok(actor_tg_id, state):
                log.warning("reassign refused on ticket %s: actor not authorised", state.tid)
                return {"ok": False, "reason": "not_authorised"}
            old = state.owner
            self.helpdesk.assign(state.tid, new_owner.uid)
            state.owner, state.reassigned = new_owner, True
            self._save(state)
            self._reissue_pending(state)
            sig = self.learning.signal("reassignment_override", {
                "ticket_id": state.tid, "client": state.client, "final_type": state.analysis.ticket_type,
                "from_uid": old.uid if old else 0, "final_owner_uid": new_owner.uid})
            return {"ok": True, "ticket_id": state.tid, "from": old.name if old else None, "to": new_owner.name, "signal": sig}
