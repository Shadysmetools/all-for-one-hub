# SPDX-License-Identifier: Apache-2.0
"""FakeHelpdesk: an in-memory, Odoo-shaped helpdesk with synthetic data only. No network.

Implements the transport protocol ``execute_kw(model, method, args, kwargs)`` + ``uid`` used by
``GuardedHelpdesk``. Every call is kept in ``calls``; every create / write / message_post in ``writes``.
``fail_on`` makes one (model, method) raise, e.g. to test "maybe created".
"""
from __future__ import annotations

import copy
from typing import Any

BOT_UID, LEAD_UID, SUPPORT_UID = 900, 778, 777
STAGE_TEMPLATES = {1: 70, 902: False, 901: False, 903: False, 9: False, 3: 71, 4: 72}
STAGE_NAMES = {1: "New", 902: "On Hold", 901: "In Progress", 903: "Waiting for Customer", 9: "Solved (auto-close)",
               3: "Closed", 4: "Cancelled"}
TICKET_FIELD_TYPES = {"name": "char", "description": "html", "team_id": "many2one", "stage_id": "many2one",
                      "tag_ids": "many2many", "partner_id": "many2one", "priority": "selection",
                      "user_id": "many2one", "x_track": "selection", "x_type": "many2one",
                      "x_ai_confidence": "float", "create_date": "datetime", "write_date": "datetime"}


def _seed() -> dict[str, dict[int, dict]]:
    tickets = {}
    for i, (track, typ, conf, prio) in enumerate([("functional", 4, 0.6, "3"), ("development", 1, 0.6, "3"),
                                                   ("functional", 5, 0.7, "0"), ("functional", 4, 0.6, "3")]):
        tid = 9001 + i
        tickets[tid] = {"id": tid, "name": f"Sample Ticket {i + 1}", "description": "<p>Request</p>", "team_id": 2,
                        "stage_id": 901, "tag_ids": [], "partner_id": 3001, "priority": prio, "user_id": LEAD_UID,
                        "x_track": track, "x_type": typ, "x_ai_confidence": conf, "create_uid": LEAD_UID}
    return {
        "helpdesk.stage": {sid: {"id": sid, "name": n, "template_id": STAGE_TEMPLATES[sid], "sequence": i}
                           for i, (sid, n) in enumerate(STAGE_NAMES.items())},
        "helpdesk.team": {2: {"id": 2, "name": "General Support (Sample)", "active": True},
                          11: {"id": 11, "name": "Product Questions (Sample)", "active": True}},
        "helpdesk.tag": {31: {"id": 31, "name": "Partner Dev"}},
        "x_ticket_type": {1: {"id": 1, "x_name": "Bug"}, 2: {"id": 2, "x_name": "New Request"},
                          3: {"id": 3, "x_name": "Customization"}, 4: {"id": 4, "x_name": "Configuration"},
                          5: {"id": 5, "x_name": "Inquiries"}},
        "res.users": {SUPPORT_UID: {"id": SUPPORT_UID, "name": "Alex Sample", "login": "alex@example.com"},
                      LEAD_UID: {"id": LEAD_UID, "name": "Maya Sample", "login": "maya@example.com"},
                      BOT_UID: {"id": BOT_UID, "name": "Hub Bot", "login": "hub@example.com"}},
        "res.partner": {3001: {"id": 3001, "name": "Contoso Retail (SAMPLE)", "is_company": True},
                        3002: {"id": 3002, "name": "Fabrikam Foods (SAMPLE)", "is_company": True}},
        "helpdesk.ticket": tickets,
        "mail.message": {},
        "mail.template": {t: {"id": t, "name": f"Template {t}"} for t in STAGE_TEMPLATES.values() if t},
    }


def _match(rec: dict, domain: list) -> bool:
    for term in domain or []:
        if not isinstance(term, (list, tuple)):
            continue                                       # '&' / '|' prefixes: the fake treats all as AND
        f, op, v = term
        x = rec.get(f)
        x = x[0] if isinstance(x, (list, tuple)) and op in ("=", "!=") and len(x) == 2 and f.endswith("_id") else x
        if op == "=" and x != v or op == "!=" and x == v:
            return False
        if op == "in" and (x not in v if not isinstance(x, list) else not set(x) & set(v)):
            return False
        if op == "not in" and x in v:
            return False
        if op == "ilike" and str(v).lower() not in str(x or "").lower():
            return False
        if op in (">=", "<=") and not (x is not None and (x >= v if op == ">=" else x <= v)):
            return False
    return True


class FakeHelpdesk:
    def __init__(self, *, uid: int = BOT_UID, data: dict | None = None):
        self.uid = uid
        self.data = data or _seed()
        self.calls: list[tuple[str, str]] = []
        self.writes: list[dict] = []
        self.fail_on: set[tuple[str, str]] = set()
        self._next = 20000

    def _id(self) -> int:
        self._next += 1
        return self._next

    def _fields_get(self, model: str) -> dict:
        types = TICKET_FIELD_TYPES if model == "helpdesk.ticket" else {"name": "char"}
        out = {k: {"type": t, "string": k} for k, t in types.items()}
        if model == "helpdesk.ticket":
            out["x_track"]["selection"] = [["functional", "Functional"], ["development", "Development"]]
        return out

    def _search(self, table: dict, method: str, args: list, kwargs: dict):
        rows = [r for r in table.values() if _match(r, args[0] if args else [])]
        if kwargs.get("order", "").endswith("desc"):
            rows = sorted(rows, key=lambda r: r["id"], reverse=True)
        if kwargs.get("limit"):
            rows = rows[: kwargs["limit"]]
        if method == "search_count":
            return len(rows)
        if method == "search":
            return [r["id"] for r in rows]
        return [self._pick(r, kwargs.get("fields")) for r in rows]

    def execute_kw(self, model: str, method: str, args: list, kwargs: dict) -> Any:
        self.calls.append((model, method))
        if (model, method) in self.fail_on:
            raise TimeoutError(f"fake timeout on {model}.{method}")
        table = self.data.setdefault(model, {})
        if method == "fields_get":
            return self._fields_get(model)
        if method in ("search_read", "search", "search_count"):
            return self._search(table, method, args, kwargs)
        if method == "read":
            return [self._pick(table[i], kwargs.get("fields")) for i in args[0] if i in table]
        if method == "create":
            vals = args[0][0] if isinstance(args[0], list) else args[0]
            rid = self._id()
            table[rid] = {"id": rid, **self._apply({}, vals), "create_uid": self.uid}
            self.writes.append({"model": model, "method": "create", "id": rid, "vals": copy.deepcopy(vals)})
            return rid
        if method == "write":
            for i in args[0]:
                table[i] = self._apply(table[i], args[1])
            self.writes.append({"model": model, "method": "write", "ids": list(args[0]),
                                "vals": copy.deepcopy(args[1])})
            return True
        if method == "message_post":
            mid = self._id()
            self.data["mail.message"][mid] = {"id": mid, "model": model, "res_id": args[0][0],
                                              "body": kwargs.get("body"), "subtype": kwargs.get("subtype_xmlid"),
                                              "author_uid": self.uid}
            self.writes.append({"model": model, "method": "message_post", "ids": list(args[0]),
                                "kwargs": dict(kwargs)})
            return mid
        raise NotImplementedError(f"FakeHelpdesk: {model}.{method}")

    @staticmethod
    def _apply(rec: dict, vals: dict) -> dict:
        out = dict(rec)
        for k, v in vals.items():
            if isinstance(v, list) and v and isinstance(v[0], (list, tuple)):     # x2many commands
                cur = list(out.get(k) or [])
                for c in v:
                    if c[0] == 4 and c[1] not in cur:
                        cur.append(c[1])
                    elif c[0] == 6:
                        cur = list(c[2])
                out[k] = cur
            else:
                out[k] = v
        return out

    @staticmethod
    def _pick(rec: dict, fields: list | None) -> dict:
        if not fields:
            return dict(rec)
        return {"id": rec["id"], **{f: rec.get(f, False) for f in fields}}

    def real_writes(self) -> int:
        return len(self.writes)
