# SPDX-License-Identifier: Apache-2.0
"""READ-ONLY Odoo adapter (search / read) over JSON-RPC, behind the guard.

This is the open-source edition's only real Odoo transport. It refuses every method that is not a read at the
transport itself, as a second lock under ``guard.check``. Real Odoo write-back (create / write tickets, notes,
assignment) is part of the commercial edition.

Credentials come from the environment (``ODOO_URL``, ``ODOO_DB``, ``ODOO_LOGIN``, ``ODOO_API_KEY``) or are passed
in explicitly; nothing here prints or logs them.
"""
from __future__ import annotations

import json
import os
from typing import Any

import httpx

from . import guard
from .gateway import HelpdeskAdapter, m2o


class OdooUnavailable(RuntimeError):
    """The adapter cannot start (missing config). The message never holds a value."""


class OdooReadTransport:
    """JSON-RPC (POST <url>/jsonrpc). Lazy: nothing connects until the first call. Reads only."""

    def __init__(self, url: str, db: str, login: str, api_key: str, *, timeout: float = 30.0, http=None):
        self.url, self.db, self._login, self._key, self.timeout = url.rstrip("/"), db, login, api_key, timeout
        self.uid = 0
        self._http = http or httpx.post

    def __repr__(self) -> str:                         # never show the key
        return f"OdooReadTransport({self.url!r}, db={self.db!r}, uid={self.uid})"

    def _rpc(self, service: str, method: str, args: list) -> Any:
        body = {"jsonrpc": "2.0", "method": "call", "id": 1,
                "params": {"service": service, "method": method, "args": args}}
        r = self._http(self.url + "/jsonrpc", content=json.dumps(body).encode(),
                       headers={"Content-Type": "application/json"}, timeout=self.timeout)
        data = r.json()
        if data.get("error"):
            err = data["error"].get("data", {}).get("name") or data["error"].get("message") or "error"
            raise RuntimeError(f"odoo {service}.{method}: {err}")      # the error class only, no payload
        return data.get("result")

    def execute_kw(self, model: str, method: str, args: list, kwargs: dict) -> Any:
        if method not in guard.READ_METHODS:
            raise guard.WriteRefused("read-only edition: write-back is part of the commercial edition")
        if not self.uid:
            self.uid = int(self._rpc("common", "authenticate", [self.db, self._login, self._key, {}]) or 0)
            if not self.uid:
                raise OdooUnavailable("Odoo login refused")
        return self._rpc("object", "execute_kw", [self.db, self.uid, self._key, model, method, args, kwargs])


def open_readonly(*, url: str = "", db: str = "", login: str = "", api_key: str = "", http=None) -> HelpdeskAdapter:
    """A guarded, writes-off adapter. Missing values fall back to the ODOO_* environment variables."""
    url = url or os.environ.get("ODOO_URL", "")
    db = db or os.environ.get("ODOO_DB", "")
    login = login or os.environ.get("ODOO_LOGIN", "")
    api_key = api_key or os.environ.get("ODOO_API_KEY", "")
    if not (url and db and login and api_key):
        raise OdooUnavailable("set ODOO_URL, ODOO_DB, ODOO_LOGIN and ODOO_API_KEY")
    return HelpdeskAdapter(OdooReadTransport(url, db, login, api_key, http=http), writes_on=False)


class OdooKnowledge:
    """Read-only ``KnowledgeSource``: a WhatsApp number -> res.partner, and the open-ticket history per partner.

    Matching assumes partner mobiles are stored without separators (``+15550100001``); override ``partner_for``
    if your instance formats numbers differently."""

    def __init__(self, adapter: HelpdeskAdapter):
        self.a = adapter

    def partner_for(self, wa_id: str) -> int | None:
        digits = "".join(ch for ch in str(wa_id) if ch.isdigit())
        if not digits:
            return None
        rows = self.a.search_read("res.partner", [["mobile", "ilike", digits[-9:]]], ["name"], limit=1)
        return int(rows[0]["id"]) if rows else None

    def client_context(self, partner_id: int) -> dict:
        rows = self.a.search_read("helpdesk.ticket", [["partner_id", "=", int(partner_id)]],
                                  ["user_id", "x_type"], limit=50, order="id desc")
        owners = sorted({int(m2o(r.get("user_id"))) for r in rows if m2o(r.get("user_id"))})
        return {"source": "odoo", "modules": [], "owner_uids": owners}
