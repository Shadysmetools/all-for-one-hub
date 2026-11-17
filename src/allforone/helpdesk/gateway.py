# SPDX-License-Identifier: Apache-2.0
"""CREATE / ASSIGN / note writes, all inside ``guard.py``.

Rules: create/write tickets, post internal notes, set assignment. NO unlink, NO other models, mail suppressed on
create via the context ``{tracking_disable, mail_create_nosubscribe, mail_create_nolog}``.

Two-layer enforcement:
  1. ``guard.check`` validates the (model, method, fields, stage) on the VALS only. The guard refuses any kwargs
     on create/write, so the mail-suppress CONTEXT is not passed through the guard; it is applied at the
     transport layer (Odoo reads ``context`` from ``execute_kw`` kwargs).
  2. The mail-suppress context is required on create here: a create missing any of the three keys (or carrying
     an extra one) is refused, so a stage email can never fire on our create.

``GuardedHelpdesk`` wraps any transport with ``execute_kw(model, method, args, kwargs)`` and ``uid``: the
in-memory ``FakeHelpdesk`` (tests, demo) or a real write transport. Real Odoo write-back: commercial edition.
The open-source package ships only the read-only transport (``odoo_read.py``).
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Protocol

from . import guard

# The mail-suppress context every create must carry.
MAIL_SUPPRESS = {"tracking_disable": True, "mail_create_nosubscribe": True, "mail_create_nolog": True}
TICKET_WANTED = ("x_track", "x_type", "x_ai_confidence", "tag_ids", "team_id", "priority", "partner_id",
                 "user_id", "stage_id")


def mail_suppress_context() -> dict:
    return dict(MAIL_SUPPRESS)


class Transport(Protocol):
    uid: int

    def execute_kw(self, model: str, method: str, args: list, kwargs: dict) -> Any: ...


def m2o(v):
    """Odoo many2one value -> id ([id, name] from RPC, an int from the fake, False when empty)."""
    if isinstance(v, (list, tuple)):
        return v[0] if v else None
    return v or None


def writes_allowed(cfg: dict, section: str = "helpdesk") -> bool:
    """Writes need ``run_mode: live`` AND ``<section>.write_enabled: true``. Anything else keeps them off."""
    return cfg.get("run_mode") == "live" and ((cfg.get(section) or {}).get("write_enabled") is True)


class HelpdeskAdapter:
    """Every call runs through ``guard.check`` first, then the transport. ``writes_on=False`` (the default)
    refuses every write even after the guard passed: defence in depth."""

    def __init__(self, transport: Transport, *, writes_on: bool = False, policy: guard.GuardPolicy | None = None):
        self.t, self.writes_on = transport, bool(writes_on)
        self.policy = policy or guard.DEFAULT_POLICY

    @property
    def uid(self) -> int:
        return int(getattr(self.t, "uid", 0) or 0)

    def call(self, purpose: str, model: str, method: str, args: list | None = None, kwargs: dict | None = None, *,
             safe_stages=(), gated_stage: int | None = None) -> Any:
        guard.check(purpose, model, method, args, kwargs, safe_stages=safe_stages, gated_stage=gated_stage,
                    policy=self.policy)
        if guard.is_write(model, method) and not self.writes_on:
            raise guard.WriteRefused("observe: writes are off (write_enabled false or not live)")
        return self.t.execute_kw(model, method, list(args or []), dict(kwargs or {}))

    def search_read(self, model: str, domain: list, fields: list[str], *, limit: int = 0, order: str = "") -> list:
        kw: dict = {"fields": fields}
        if limit:
            kw["limit"] = limit
        if order:
            kw["order"] = order
        return self.call("read", model, "search_read", [domain], kw) or []

    def read(self, model: str, ids: list[int], fields: list[str]) -> list:
        return self.call("read", model, "read", [ids], {"fields": fields}) or []

    def fields_get(self, model: str) -> dict:
        return self.call("read", model, "fields_get", [], {"attributes": ["type", "relation", "selection",
                                                                          "string", "readonly"]}) or {}


@dataclass
class Conventions:
    """Field conventions of the helpdesk, discovered at runtime (the live database is the check, not a doc)."""

    ticket_fields: set[str] = field(default_factory=set)
    track_values: list[str] = field(default_factory=list)
    stages: dict[int, dict] = field(default_factory=dict)       # id -> {name, template_id, emails_client}
    safe_stages: set[int] = field(default_factory=set)
    teams: dict[int, dict] = field(default_factory=dict)        # id -> {name}
    tags: dict[str, int] = field(default_factory=dict)          # lower name -> id
    types: dict[str, int] = field(default_factory=dict)         # lower name -> ticket type id
    uid: int = 0
    problems: list[str] = field(default_factory=list)

    def emails_client(self, stage_id: int) -> bool:
        st = self.stages.get(int(stage_id))
        return True if st is None else bool(st.get("emails_client"))   # unknown stage = assume it emails

    def has(self, f: str) -> bool:
        return f in self.ticket_fields


def _safe(call, problems: list[str], what: str, default):
    try:
        return call()
    except Exception as e:                                     # noqa: BLE001 - discovery never crashes the hub
        problems.append(f"{what}: {type(e).__name__}")
        return default


def _discover_stages(adapter: HelpdeskAdapter, c: Conventions, hd: dict) -> None:
    stages = _safe(lambda: adapter.search_read("helpdesk.stage", [], ["name", "template_id", "sequence"]),
                   c.problems, "stages", None)
    wanted = {int(s) for s in hd.get("safe_stage_ids") or ()}
    always_emailing = {int(s) for s in hd.get("client_emailing_stage_ids") or ()}
    closed = {int(s) for s in hd.get("closed_stage_ids") or ()}
    if stages is None:
        c.safe_stages = set()                                  # fail closed: no stage move at all
        return
    for s in stages:
        sid = int(s["id"])
        emails = bool(m2o(s.get("template_id"))) or sid in always_emailing
        c.stages[sid] = {"name": s.get("name"), "template_id": m2o(s.get("template_id")), "emails_client": emails,
                         "closed": sid in closed}
    c.safe_stages = {sid for sid in wanted if sid in c.stages and not c.stages[sid]["emails_client"]}
    for sid in sorted(wanted - c.safe_stages):
        c.problems.append(f"stage {sid} is not confirmed silent at runtime: moves into it need a human")


def discover(adapter: HelpdeskAdapter, cfg: dict | None = None) -> Conventions:
    """Read the live field set, stages, teams, tags and ticket types. ``cfg['helpdesk']['safe_stage_ids']`` lists
    the stages that may be entered without a human; each must ALSO have no mail template at runtime."""
    hd = (cfg or {}).get("helpdesk") or {}
    c = Conventions()
    tf = _safe(lambda: adapter.fields_get("helpdesk.ticket"), c.problems, "ticket fields", {})
    c.ticket_fields = set(tf)
    c.track_values = [s[0] for s in (tf.get("x_track") or {}).get("selection") or []]
    for f in TICKET_WANTED:
        if c.ticket_fields and f not in c.ticket_fields:
            c.problems.append(f"helpdesk.ticket has no field {f}: it will not be written")
    _discover_stages(adapter, c, hd)
    for t in _safe(lambda: adapter.search_read("helpdesk.team", [["active", "=", True]], ["name"]), c.problems,
                   "teams", []):
        c.teams[int(t["id"])] = {"name": t.get("name")}
    c.tags = {str(t.get("name") or "").lower(): int(t["id"]) for t in
              _safe(lambda: adapter.search_read("helpdesk.tag", [], ["name"]), c.problems, "tags", [])}
    if c.has("x_type"):
        c.types = {str(t.get("x_name") or "").lower(): int(t["id"]) for t in
                   _safe(lambda: adapter.search_read("x_ticket_type", [], ["x_name"]), c.problems, "ticket types", [])}
    c.uid = adapter.uid
    return c


class GuardedHelpdesk:
    """Wraps a transport. Writes are OFF unless ``writes_on`` is True; build it with ``from_config`` so the
    ``run_mode: live`` + ``write_enabled`` gate decides, never a default."""

    def __init__(self, transport: Transport, *, conv: Conventions | None = None, writes_on: bool = False,
                 cfg: dict | None = None, policy: guard.GuardPolicy | None = None):
        self.t = transport
        self.adapter = HelpdeskAdapter(transport, writes_on=writes_on, policy=policy)
        self.conv = conv or discover(self.adapter, cfg)

    @classmethod
    def from_config(cls, transport: Transport, cfg: dict, *, section: str = "helpdesk",
                    conv: Conventions | None = None):
        return cls(transport, conv=conv, writes_on=writes_allowed(cfg or {}, section), cfg=cfg)

    @property
    def safe_stages(self):
        return self.conv.safe_stages

    def create_ticket(self, vals: dict, *, context: dict) -> int:
        if context != MAIL_SUPPRESS:                 # EXACT: nothing missing, nothing extra (e.g. force_send)
            raise guard.WriteRefused("create refused: context must equal the mail-suppress context exactly")
        guard.check("helpdesk", "helpdesk.ticket", "create", [vals], safe_stages=self.conv.safe_stages,
                    policy=self.adapter.policy)
        if not self.adapter.writes_on:
            raise guard.WriteRefused("observe: writes are off")
        return int(self.t.execute_kw("helpdesk.ticket", "create", [vals], {"context": context}))

    def assign(self, tid: int, user_id: int) -> None:
        args = [[int(tid)], {"user_id": int(user_id)}]
        guard.check("helpdesk", "helpdesk.ticket", "write", args, safe_stages=self.conv.safe_stages,
                    policy=self.adapter.policy)
        if not self.adapter.writes_on:
            raise guard.WriteRefused("observe: writes are off")
        self.t.execute_kw("helpdesk.ticket", "write", args, {"context": mail_suppress_context()})

    def post_note(self, tid: int, body: str) -> None:
        kw = {"body": body, "subtype_xmlid": "mail.mt_note", "message_type": "comment"}
        guard.check("helpdesk", "helpdesk.ticket", "message_post", [[int(tid)]], kw, policy=self.adapter.policy)
        if not self.adapter.writes_on:
            raise guard.WriteRefused("observe: writes are off")
        self.t.execute_kw("helpdesk.ticket", "message_post", [[int(tid)]], kw)

    def search_read(self, model, domain, fields, **kw):
        return self.adapter.search_read(model, domain, fields, **kw)

    def open_tickets_for(self, user_id: int) -> int:
        """Current open load for a candidate (routing): tickets not in a closed stage."""
        rows = self.adapter.search_read("helpdesk.ticket", [["user_id", "=", int(user_id)]], ["stage_id"])
        closed = {sid for sid, st in self.conv.stages.items() if st.get("closed")}
        return sum(1 for r in rows if m2o(r.get("stage_id")) not in closed)


def team_id_for(track: str, conv: Conventions, teams: dict | None = None) -> int:
    """Pick the helpdesk team for a track. ``teams`` comes from config: ``{"default": 2, "inquiry": 11}``;
    a configured id that does not exist in the live helpdesk falls back to the default, then to any team."""
    teams = teams or {}
    want = int(teams.get("inquiry" if track == "inquiry" else "default", 0) or 0)
    default = int(teams.get("default", 0) or 0)
    if want in conv.teams:
        return want
    if default in conv.teams:
        return default
    return next(iter(conv.teams), default or 1)


def x_type_id(ticket_type: str, conv: Conventions) -> int | None:
    return conv.types.get(str(ticket_type).lower())


def x_track_value(track: str, conv: Conventions) -> str:
    """Map the analysis track onto a value the live ``x_track`` selection actually offers."""
    vals = set(conv.track_values or [])
    if track in vals:
        return track
    return "functional" if "functional" in vals else (next(iter(vals)) if vals else "functional")


__all__ = ["GuardedHelpdesk", "HelpdeskAdapter", "Conventions", "discover", "MAIL_SUPPRESS"]
