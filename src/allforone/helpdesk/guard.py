# SPDX-License-Identifier: Apache-2.0
"""The helpdesk write guard: an allowlist of (model, method, fields) enforced in code, above every transport.

Never AI. Every adapter and the in-memory fake go through the same ``check()``, so a test of one is a test of
the other.

- Reads: only the models in ``read_models`` and the methods in ``READ_METHODS``.
- Writes (purpose ``"helpdesk"``): ticket create / write (exactly one record) and ``message_post`` as an
  internal NOTE only.
- Never: ``unlink``, private methods, any ``ir.*`` / ``base.*`` model, a field outside the allowlist, a
  forbidden field, or a stage outside the safe set.
- A ticket may only enter a stage that does not email the client (the *safe set*, confirmed at runtime). The one
  exception is the exact stage a human approved (``gated_stage``).
- ``create`` without ``stage_id`` would land in the default first stage, which usually emails the client: refused.

The policy is data (``GuardPolicy``), so a deployment can widen the field allowlist for its own custom fields
without touching the rules.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Iterable

READ_METHODS = frozenset({"fields_get", "search_read", "read", "search_count", "search"})
NOTE_KWARGS = frozenset({"body", "subtype_xmlid", "message_type", "body_is_html"})
WRITES = {"helpdesk": frozenset({("helpdesk.ticket", "create"), ("helpdesk.ticket", "write"),
                                 ("helpdesk.ticket", "message_post")})}


@dataclass(frozen=True)
class GuardPolicy:
    read_models: frozenset = frozenset({"helpdesk.ticket", "helpdesk.stage", "helpdesk.team", "helpdesk.tag",
                                        "x_ticket_type", "res.partner", "res.users", "mail.template",
                                        "mail.message"})
    ticket_fields: frozenset = frozenset({"name", "description", "team_id", "stage_id", "tag_ids", "partner_id",
                                          "priority", "user_id", "x_track", "x_type", "x_ai_confidence"})
    forbidden_fields: frozenset = field(default_factory=frozenset)

    def with_custom_fields(self, *names: str, forbidden: Iterable[str] = ()) -> "GuardPolicy":
        return GuardPolicy(self.read_models, self.ticket_fields | frozenset(names),
                           self.forbidden_fields | frozenset(forbidden))


DEFAULT_POLICY = GuardPolicy()


class WriteRefused(PermissionError):
    """The guard said no. The message names the rule, never a value."""


def is_write(model: str, method: str) -> bool:
    return method not in READ_METHODS


def _vals_of(method: str, args: list) -> dict:
    if method == "create":
        v = args[0] if args else None
        if isinstance(v, list):                      # create([vals]) form: exactly one record
            if len(v) != 1:
                raise WriteRefused("create: exactly one record per call")
            v = v[0]
        if not isinstance(v, dict):
            raise WriteRefused("create: vals must be a dict")
        return v
    if method == "write":
        if len(args) != 2 or not isinstance(args[1], dict):
            raise WriteRefused("write: (ids, vals) expected")
        return args[1]
    return {}


def _one_id(args: list, what: str) -> int:
    ids = args[0] if args else None
    if not isinstance(ids, list) or len(ids) != 1 or not isinstance(ids[0], int) or ids[0] <= 0:
        raise WriteRefused(f"{what}: exactly one record id")
    return ids[0]


def _check_x2many(name: str, cmds: Any, *, on_create: bool) -> None:
    """Link-only commands: (4, id) adds a link; (6, 0, ids) only on create. Never create/update/delete/unlink."""
    if not isinstance(cmds, list):
        raise WriteRefused(f"{name}: a command list is required")
    for c in cmds:
        ok = (isinstance(c, (list, tuple)) and len(c) >= 2 and c[0] == 4 and isinstance(c[1], int)) or \
             (on_create and isinstance(c, (list, tuple)) and len(c) == 3 and c[0] == 6 and isinstance(c[2], list))
        if not ok:
            raise WriteRefused(f"{name}: only link commands (4, id) are allowed")


def _check_fields(vals: dict, policy: GuardPolicy, what: str) -> None:
    bad = set(vals) & policy.forbidden_fields
    if bad:
        raise WriteRefused(f"{what}: forbidden field {sorted(bad)[0]}")
    extra = set(vals) - policy.ticket_fields
    if extra:
        raise WriteRefused(f"{what}: field not on the allowlist: {', '.join(sorted(extra))}")


def _check_stage(vals: dict, safe_stages: set[int], gated_stage: int | None, *, required: bool) -> None:
    if "stage_id" not in vals:
        if required:
            raise WriteRefused("create: stage_id is required (the first stage emails the client)")
        return
    sid = vals["stage_id"]
    if not isinstance(sid, int):
        raise WriteRefused("stage_id must be an id")
    if sid in safe_stages or (gated_stage is not None and sid == gated_stage):
        return
    raise WriteRefused(f"stage {sid} emails the client or is not confirmed silent: needs a human approval")


def _check_ticket_write(method: str, args: list, policy: GuardPolicy, safe: set[int],
                        gated_stage: int | None) -> None:
    if method == "write":
        _one_id(args, "write")
    vals = _vals_of(method, args)
    _check_fields(vals, policy, f"helpdesk.ticket.{method}")
    if method == "write" and "partner_id" in vals:
        raise WriteRefused("helpdesk.ticket.write: the client (partner_id) is set at create only")
    if method == "write" and "team_id" in vals:
        raise WriteRefused("helpdesk.ticket.write: team_id is not writable (a team change can recompute the "
                           "stage to a client-emailing one); change the team at create only")
    _check_stage(vals, safe, gated_stage, required=method == "create")
    if "tag_ids" in vals:
        _check_x2many("tag_ids", vals["tag_ids"], on_create=method == "create")
    conf = vals.get("x_ai_confidence")
    if conf is not None and not (isinstance(conf, (int, float)) and 0 <= conf <= 1):
        raise WriteRefused("x_ai_confidence must be between 0 and 1")


def _check_note(args: list, kwargs: dict) -> None:
    _one_id(args, "message_post")
    extra = set(kwargs) - NOTE_KWARGS
    if extra:
        raise WriteRefused(f"message_post: not allowed: {', '.join(sorted(extra))} (internal notes only)")
    if kwargs.get("subtype_xmlid") != "mail.mt_note" or kwargs.get("message_type", "comment") != "comment":
        raise WriteRefused("message_post: only an internal note (mail.mt_note, comment)")
    if not str(kwargs.get("body") or "").strip():
        raise WriteRefused("message_post: empty note")


def check(purpose: str, model: str, method: str, args: list | None = None, kwargs: dict | None = None, *,
          safe_stages: Iterable[int] = (), gated_stage: int | None = None,
          policy: GuardPolicy = DEFAULT_POLICY) -> None:
    """Raise WriteRefused unless this exact call is allowed. Returns None when it is."""
    args, kwargs = list(args or []), dict(kwargs or {})
    model, method = str(model or ""), str(method or "")
    if method == "unlink" or method.startswith("_"):
        raise WriteRefused(f"{method or 'method'} is never allowed")
    if model.startswith("ir.") or model.startswith("base."):
        raise WriteRefused(f"model {model} is never allowed")
    if method in READ_METHODS:
        if model not in policy.read_models:
            raise WriteRefused(f"read of {model} is not on the allowlist")
        return
    if (model, method) not in WRITES.get(purpose, frozenset()):
        raise WriteRefused(f"{model}.{method} is not allowed for purpose {purpose!r}")
    if method in ("create", "write") and kwargs:
        raise WriteRefused(f"{model}.{method}: extra keyword arguments are not allowed")
    safe = {int(s) for s in safe_stages}
    if model == "helpdesk.ticket" and method in ("create", "write"):
        _check_ticket_write(method, args, policy, safe, gated_stage)
        return
    if model == "helpdesk.ticket" and method == "message_post":
        _check_note(args, kwargs)
        return
    raise WriteRefused(f"{model}.{method} is not allowed")
