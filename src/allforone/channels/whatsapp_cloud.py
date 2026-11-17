# SPDX-License-Identifier: Apache-2.0
"""WhatsApp Business Cloud API adapter (official Meta Graph API only; no unofficial web libraries).

What it can do:
- ``send_text(to, text, last_inbound_at=, now=)``: free-form text, only inside the 24 h customer-service window
- ``send_template(to, name, lang, params, now=)``: an approved template, the only thing allowed outside the window
- ``download_media(media_id)``: fetch an inbound voice note / media file (used by the STT adapter)

Safety: the adapter refuses unless it is enabled, has a token and a phone number id, the recipient is on
``allowed_recipients`` (empty list = nobody), and the kill switch is off. ``to`` is never logged or put in an
exception; HTTP errors carry only the status and Meta's numeric error code.
"""
from __future__ import annotations

from datetime import datetime, timedelta
from typing import Callable

import httpx

GRAPH = "https://graph.facebook.com/{version}/{phone_id}/{edge}"
MEDIA = "https://graph.facebook.com/{version}/{media_id}"


class WhatsAppError(RuntimeError):
    """The API said no or the call failed. Never carries the recipient or the message text."""


class WhatsAppRefused(WhatsAppError):
    """The adapter's own safety check said no (nothing went out)."""


class WindowClosed(WhatsAppRefused):
    """Outside the 24 h customer-service window: only an approved template may be sent."""


def in_service_window(last_inbound_at: datetime | None, now: datetime, hours: float = 24) -> bool:
    """True if the customer wrote to us less than ``hours`` ago (Meta's customer-service window)."""
    if last_inbound_at is None:
        return False
    age = now - last_inbound_at
    return timedelta(0) <= age < timedelta(hours=hours)


def _digits(number) -> str:
    return "".join(ch for ch in str(number or "") if ch.isdigit())


def _httpx_request(method: str, url: str, *, headers: dict, json: dict | None = None,
                   timeout: float = 30) -> tuple[int, dict]:
    r = httpx.request(method, url, headers=headers, json=json, timeout=timeout)
    try:
        body = r.json()
    except ValueError:
        body = {}
    return r.status_code, body if isinstance(body, dict) else {}


class WhatsAppCloud:
    def __init__(self, *, token: str, phone_number_id: str, graph_version: str, allowed_recipients: list[str],
                 enabled: bool = False, window_hours: float = 24, http: Callable[..., tuple[int, dict]] | None = None,
                 kill_switch: Callable[[], bool] | None = None):
        if not enabled:
            raise WhatsAppRefused("whatsapp is not enabled")
        if not token or not phone_number_id or not str(graph_version).strip():
            raise WhatsAppRefused("token, phone_number_id and graph_version are required")
        self.__token = str(token)
        self.phone_id, self.version = str(phone_number_id), str(graph_version).strip()
        self.allowed = {_digits(x) for x in allowed_recipients or []} - {""}
        self.window_hours = float(window_hours)
        self._http = http or _httpx_request
        self._kill = kill_switch or (lambda: False)

    def __repr__(self) -> str:
        return "WhatsAppCloud()"

    def _check(self, to, *, last_inbound_at: datetime | None, now: datetime, template: bool = False) -> str:
        if self._kill():
            raise WhatsAppRefused("kill switch is on")
        number = _digits(to)
        if not number or number not in self.allowed:
            raise WhatsAppRefused("recipient is not on allowed_recipients")
        if not template and not in_service_window(last_inbound_at, now, self.window_hours):
            raise WindowClosed("outside the 24 h customer-service window: approved template only")
        return number

    def _headers(self) -> dict:
        return {"Authorization": f"Bearer {self.__token}"}

    def _call(self, method: str, url: str, **kw) -> dict:
        try:
            status, body = self._http(method, url, headers=self._headers(), timeout=30, **kw)
        except Exception as e:  # noqa: BLE001 - network: we cannot know if it went out (maybe_sent)
            raise WhatsAppError(f"network: {type(e).__name__}") from None
        if status != 200:
            code = ((body or {}).get("error") or {}).get("code")
            raise WhatsAppError(f"http {status}" + (f" code {code}" if code is not None else ""))
        return body or {}

    def _message(self, number: str, payload: dict) -> dict:
        url = GRAPH.format(version=self.version, phone_id=self.phone_id, edge="messages")
        body = self._call("POST", url, json={"messaging_product": "whatsapp", "recipient_type": "individual",
                                             "to": number, **payload})
        ids = [m.get("id") for m in body.get("messages") or [] if isinstance(m, dict)]
        if not ids or not ids[0]:
            raise WhatsAppError("no message id in the answer")
        return {"msg_id": str(ids[0])}

    def send_text(self, to, text: str, *, last_inbound_at: datetime | None, now: datetime,
                  automatic: bool = False) -> dict:
        number = self._check(to, last_inbound_at=last_inbound_at, now=now)
        return self._message(number, {"type": "text", "text": {"body": text, "preview_url": False}})

    def send_template(self, to, name: str, lang: str, params: list[str] | None = None, *, now: datetime) -> dict:
        """An approved template (any time, also outside the window). Only body text parameters."""
        number = self._check(to, last_inbound_at=None, now=now, template=True)
        template = {"name": name, "language": {"code": lang}}
        if params:
            template["components"] = [{"type": "body",
                                       "parameters": [{"type": "text", "text": str(p)} for p in params]}]
        return self._message(number, {"type": "template", "template": template})

    def download_media(self, media_id: str) -> bytes:
        """Two Graph calls: media id -> short-lived URL -> bytes. Raises ``WhatsAppError`` on any failure."""
        meta = self._call("GET", MEDIA.format(version=self.version, media_id=media_id))
        url = str(meta.get("url") or "")
        if not url.startswith("https://"):
            raise WhatsAppError("no media url in the answer")
        try:
            r = httpx.get(url, headers=self._headers(), timeout=60)
        except Exception as e:  # noqa: BLE001
            raise WhatsAppError(f"network: {type(e).__name__}") from None
        if r.status_code != 200:
            raise WhatsAppError(f"http {r.status_code}")
        return r.content
