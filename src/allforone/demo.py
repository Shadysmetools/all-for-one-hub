# SPDX-License-Identifier: Apache-2.0
"""``python -m allforone.demo``: the full client loop, end to end, on fakes, with a deterministic clock.

Thirteen steps: a voice note arrives, is transcribed, analysed, becomes a ticket, is assigned, gets the instant
ack, a CONFIRM draft is judged and approved by the owner in (fake) Telegram, the client confirms, the proof run
produces a (fake) share link, the RESOLVE draft is approved and sent, and the KPIs land.

Nothing here touches a network or a credential. Output is colorized with plain ANSI codes; a JSON event timeline
is written to ``demo/out/timeline.json`` for the video renderer. ``--lang ar`` runs the Arabic variant.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time
from datetime import UTC, datetime, timedelta
from importlib import resources
from pathlib import Path

from .config import demo_config
from .wiring import build_fake_hub

START = datetime(2026, 1, 12, 9, 0, 0, tzinfo=UTC)
WA_ID = "15550100001"
AUDIO_ID = "MEDIA_AUDIO_0001"
FIXTURES = Path(__file__).parent / "fixtures"

SCRIPTS = {
    "en": {"voice": "The stock quantity in Inventory is wrong after I validated a delivery",
           "confirm": "Yes, that is exactly it. Please go ahead."},
    "ar": {"voice": "عندي مشكلة في المخزون، الكمية ظاهرة غلط بعد ما عملت تسليم",
           "confirm": "أيوه، كده صح. من فضلكم كملوا."},
}
STEP_NAMES = {1: "Voice note arrives", 2: "Transcription", 3: "Analysis", 4: "Ticket created", 5: "Assignment",
              6: "Instant ack", 7: "CONFIRM draft", 8: "Judge score", 9: "Owner approves", 10: "Client confirms",
              11: "Proof run", 12: "RESOLVE approved and sent", 13: "KPIs"}


class DemoClock:
    """A deterministic clock: it only moves when the script moves it."""

    def __init__(self, start: datetime = START):
        self.now = start

    def __call__(self) -> datetime:
        return self.now

    def advance(self, **kw) -> None:
        self.now += timedelta(**kw)


class Printer:
    """Pretty step output with stdlib ANSI only (colors off when not a TTY, NO_COLOR is set, or --no-color)."""

    CODES = {"dim": "2", "bold": "1", "cyan": "36", "green": "32", "yellow": "33", "magenta": "35", "blue": "34"}

    def __init__(self, color: bool, pause: float = 0.0, out=None):
        self.color, self.pause, self.out = color, pause, out or sys.stdout

    def c(self, text: str, *styles: str) -> str:
        if not self.color:
            return text
        return "".join(f"\033[{self.CODES[s]}m" for s in styles) + text + "\033[0m"

    def line(self, text: str = "") -> None:
        print(text, file=self.out)

    def step(self, n: int, actor: str, summary: str) -> None:
        tag = self.c(f"[{n:>2}/13]", "bold", "cyan")
        who = self.c(f"{actor:<7}", "magenta")
        self.line(f"{tag} {who} {self.c(STEP_NAMES[n], 'bold')}: {summary}")
        if self.pause:
            self.out.flush()
            time.sleep(self.pause)

    def detail(self, label: str, value: str) -> None:
        self.line(f"        {self.c(label + ':', 'dim')} {value}")


class Timeline:
    """Collects the JSON events for the video renderer."""

    def __init__(self, clock: DemoClock, lang: str):
        self.clock, self.lang, self.events = clock, lang, []

    def add(self, step: int, actor: str, summary: str, payload: dict) -> None:
        self.events.append({"t": self.clock().isoformat(), "offset_s": (self.clock() - START).total_seconds(),
                            "step": step, "name": STEP_NAMES[step], "actor": actor, "summary": summary,
                            "payload": payload})

    def document(self) -> dict:
        return {"demo": "allforone", "lang": self.lang, "start": START.isoformat(),
                "steps": len(STEP_NAMES), "events": self.events}


def _voice_payload(ts: datetime) -> dict:
    p = json.loads((FIXTURES / "webhook_voice.json").read_text(encoding="utf-8"))
    value = p["entry"][0]["changes"][0]["value"]
    value["messages"][0].update({"id": "wamid.DEMO0001", "timestamp": str(int(ts.timestamp()))})
    value["contacts"][0]["profile"]["name"] = "Jordan"
    return p


def _text_payload(ts: datetime, text: str) -> dict:
    p = json.loads((FIXTURES / "webhook_confirm.json").read_text(encoding="utf-8"))
    msg = p["entry"][0]["changes"][0]["value"]["messages"][0]
    msg.update({"id": "wamid.DEMO0002", "timestamp": str(int(ts.timestamp()))})
    p["entry"][0]["changes"][0]["value"]["contacts"][0]["profile"]["name"] = "Jordan"
    msg["text"]["body"] = text
    return p


def _placeholder_audio() -> dict:
    raw = resources.files("allforone").joinpath("data/voice_note_placeholder.wav").read_bytes()
    return {"bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()[:12], "synthetic": True}


class Demo:
    def __init__(self, lang: str, printer: Printer):
        from . import fakes
        self.lang, self.p = lang, printer
        self.clock = DemoClock()
        self.tl = Timeline(self.clock, lang)
        cfg = {**demo_config(), "phone_number_id": "PHONE_NUMBER_ID"}
        notes = {3001: {"source": "notes", "name": "Contoso", "modules": ["Inventory"], "owner_uids": [778],
                        "agreed": ["monthly stock review"], "usual_collaborators": ["Maya"]}}
        self.hub = build_fake_hub(
            cfg, clock=self.clock, stt=fakes.FakeSTT({AUDIO_ID: SCRIPTS[lang]["voice"]}),
            knowledge_notes=fakes.FakeNotesKnowledge(notes))
        self.owner_tg = 1003
        self.tid = 0

    # -- helpers -------------------------------------------------------------------------------------------

    def emit(self, step: int, actor: str, summary: str, payload: dict, details: dict | None = None) -> None:
        self.p.step(step, actor, summary)
        for k, v in (details or {}).items():
            self.p.detail(k, str(v))
        self.tl.add(step, actor, summary, payload)

    def tap(self, kind: str) -> dict:
        req = next(r for r in reversed(self.hub.telegram.approvals) if r["kind"] == kind and
                   not (self.hub.store.get("approvals", r["nonce"]) or {}).get("used_at"))
        return self.hub.on_owner_decision(req["nonce"], self.owner_tg, approved=True)

    def sent(self) -> list[dict]:
        return self.hub.whatsapp.sent

    # -- the loop ------------------------------------------------------------------------------------------

    def run(self) -> dict:
        self.p.line(self.p.c(f"All for One demo ({self.lang.upper()}) | fictional company: Northwind Supplies", "bold"))
        self.p.line(self.p.c("fakes only: no network, no credentials, nothing is sent to a real client", "dim"))
        self.p.line()
        res = self.client_voice_note()
        self.analyse_and_ticket(res)
        self.ack_and_confirm_draft()
        self.owner_approves_confirm()
        self.client_confirms()
        self.prove_and_resolve()
        self.kpis()
        return self.tl.document()

    def client_voice_note(self) -> dict:
        audio = _placeholder_audio()
        self.emit(1, "client", f"voice note from +1-555-0100 ({audio['bytes']} byte synthetic clip)",
                  {"from": "+1-555-0100", "kind": "voice", "audio": audio})
        self.clock.advance(seconds=1)
        res = self.hub.handle_webhook(_voice_payload(self.clock()))
        stt = next(s for s in res["log"] if s["stage"] == "2-STT")
        text = self.hub.ticket(res["ticket_id"]).inbound.text
        self.emit(2, "hub", f"speech-to-text ({stt['provider']})", {"transcript": text, "provider": stt["provider"]},
                  {"transcript": text})
        return res

    def analyse_and_ticket(self, res: dict) -> None:
        a = next(s for s in res["log"] if s["stage"] == "3-ANALYSE")
        self.tid = int(res["ticket_id"])
        self.clock.advance(seconds=1)
        self.emit(3, "hub", f"{a['nature']} | type {a['ticket_type']} | module {a['module']} | track {a['track']}",
                  {k: a[k] for k in ("nature", "ticket_type", "module", "track")},
                  {"urgency": "normal (priority 1)"})
        write = next(w for w in self.hub.helpdesk.t.writes if w["method"] == "create")
        self.emit(4, "hub", f"helpdesk ticket #{self.tid} created (mail suppressed, silent stage "
                  f"{write['vals']['stage_id']})", {"ticket_id": self.tid, "team_id": write["vals"]["team_id"],
                                                   "stage_id": write["vals"]["stage_id"]})
        st = self.hub.ticket(self.tid)
        self.emit(5, "hub", f"assigned to {st.owner.name}", {"owner": st.owner.name, "uid": st.owner.uid,
                                                            "rationale": st.rationale}, {"why": st.rationale})

    def ack_and_confirm_draft(self) -> None:
        self.clock.advance(seconds=1)
        ack = self.sent()[0]
        self.emit(6, "hub", "automatic acknowledgement sent (the only automatic message)",
                  {"text": ack["text"], "automatic": ack["automatic"]}, {"message": ack["text"]})
        req = self.hub.telegram.approvals[-1]
        self.clock.advance(seconds=2)
        self.emit(7, "hub", f"draft held for owner approval in Telegram (ticket #{self.tid})",
                  {"kind": req["kind"], "draft": req["draft"], "sent_to_client": False},
                  {"draft": req["draft"]})
        j = next(s for s in self.hub.judge.scored if s["kind"] == "draft")
        self.emit(8, "judge", f"aggregate {j['aggregate']} (threshold {j['threshold']}), "
                  f"flag={j['flag']}", {"scores": j["scores"], "aggregate": j["aggregate"], "flag": j["flag"]},
                  {"scores": j["scores"]})

    def owner_approves_confirm(self) -> None:
        self.clock.advance(seconds=45)
        before = len(self.sent())
        self.tap("confirm")
        msg = self.sent()[before]
        self.emit(9, "owner", f"{self.hub.ticket(self.tid).owner.name} taps Approve; CONFIRM sent to the client",
                  {"approver_tg_id": self.owner_tg, "sent": msg["text"], "automatic": msg["automatic"]})

    def client_confirms(self) -> None:
        self.clock.advance(minutes=3)
        text = SCRIPTS[self.lang]["confirm"]
        self.hub.handle_webhook(_text_payload(self.clock(), text))
        self.clock.advance(seconds=20)
        self.tap("advance")
        self.emit(10, "client", "client confirms; the owner marks it confirmed and work starts",
                  {"reply": text, "stage": self.hub.ticket(self.tid).stage}, {"client says": text})

    def prove_and_resolve(self) -> None:
        self.clock.advance(hours=2)
        self.tap("fix_ready")
        st = self.hub.ticket(self.tid)
        self.emit(11, "proof", f"{st.proof['verdict']}: share link {st.proof['share_link']}",
                  {"verdict": st.proof["verdict"], "share_link": st.proof["share_link"], "simulated": True},
                  {"video judge": st.proof_judge})
        self.clock.advance(seconds=30)
        before = len(self.sent())
        self.tap("resolve")
        msg = self.sent()[before]
        self.emit(12, "owner", "RESOLVE draft approved and sent; ticket resolved",
                  {"sent": msg["text"], "stage": self.hub.ticket(self.tid).stage}, {"message": msg["text"]})

    def kpis(self) -> None:
        self.clock.advance(seconds=1)
        outcome = self.hub.kpi.pushed[-1]["outcome"]
        auto = self.hub.whatsapp.automatic_count()
        self.emit(13, "hub", f"resolved, {auto} automatic message, {len(self.sent())} client messages in total",
                  {"outcome": outcome, "automatic_messages": auto, "client_messages": len(self.sent())},
                  {"kpi": outcome})


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="allforone.demo", description="Run the All for One client loop on fakes.")
    ap.add_argument("--lang", choices=("en", "ar"), default="en")
    ap.add_argument("--out", default="demo/out", help="directory for timeline.json")
    ap.add_argument("--no-color", action="store_true")
    ap.add_argument("--pause", type=float, default=0.0, help="seconds to pause after each step (for recordings)")
    args = ap.parse_args(argv)
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    color = not args.no_color and not os.environ.get("NO_COLOR") and sys.stdout.isatty()
    if os.environ.get("FORCE_COLOR") and not args.no_color:
        color = True
    doc = Demo(args.lang, Printer(color, args.pause)).run()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / "timeline.json").write_text(json.dumps(doc, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"\ntimeline written to {out / 'timeline.json'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
