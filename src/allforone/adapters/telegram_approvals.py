# SPDX-License-Identifier: Apache-2.0
"""Telegram owner-approval channel over the Bot API (extra: ``pip install allforone[telegram]``).

Implements the ``TelegramGateway`` protocol: one forum topic per ticket, a triage chat, and approval requests as
inline buttons that carry only the nonce (``a:<nonce>`` approve, ``d:<nonce>`` decline). The hub keeps the draft
and validates the tap, so nothing the callback echoes back can change what is sent.

``handle_update(hub, update)`` turns a ``callback_query`` update into ``Hub.on_owner_decision`` with the tapping
user's Telegram id passed through unchanged. The bot token is never logged.
"""
from __future__ import annotations

from typing import Any, Callable

import httpx

API = "https://api.telegram.org/bot{token}/{method}"


class TelegramError(RuntimeError):
    """A Bot API call failed. Carries the method and status only, never the token or message text."""


def _post(url: str, payload: dict) -> dict:
    r = httpx.post(url, json=payload, timeout=20)
    try:
        body = r.json()
    except ValueError:
        body = {}
    if r.status_code != 200 or not body.get("ok"):
        raise TelegramError(f"http {r.status_code}")
    return body.get("result") or {}


class TelegramApprovals:
    def __init__(self, *, token: str, chat_id: int, triage_chat_id: int = 0,
                 post: Callable[[str, dict], dict] | None = None):
        if not token or not chat_id:
            raise ValueError("token and chat_id are required")
        self.__token, self.chat_id = token, int(chat_id)
        self.triage_chat_id = int(triage_chat_id or chat_id)
        self._post = post or _post

    def __repr__(self) -> str:
        return "TelegramApprovals()"

    def _call(self, method: str, payload: dict) -> dict:
        try:
            return self._post(API.format(token=self.__token, method=method), payload)
        except TelegramError:
            raise TelegramError(method) from None
        except Exception as e:                                   # noqa: BLE001
            raise TelegramError(f"{method}: {type(e).__name__}") from None

    def open_topic(self, tid: int, owner: Any, brief: str) -> int:
        name = f"#{tid} {getattr(owner, 'name', owner)}"
        return int(self._call("createForumTopic", {"chat_id": self.chat_id, "name": name})["message_thread_id"])

    def post(self, topic_id: int, text: str) -> None:
        self._call("sendMessage", {"chat_id": self.chat_id, "message_thread_id": topic_id, "text": text})

    def post_triage(self, text: str) -> None:
        self._call("sendMessage", {"chat_id": self.triage_chat_id, "text": text})

    def request_approval(self, topic_id: int, *, ticket_id: int, kind: str, draft: str, nonce: str,
                         draft_hash: str, allowed_approvers: list[int], expires_at: Any) -> dict:
        keyboard = {"inline_keyboard": [[{"text": "Approve", "callback_data": f"a:{nonce}"},
                                         {"text": "Decline", "callback_data": f"d:{nonce}"}]]}
        text = f"[{kind}] ticket #{ticket_id}\n{draft}"
        self._call("sendMessage", {"chat_id": self.chat_id, "message_thread_id": topic_id, "text": text,
                                   "reply_markup": keyboard})
        return {"status": "pending", "nonce": nonce}


def handle_update(hub: Any, update: dict) -> dict | None:
    """Route a Telegram ``callback_query`` to the hub. Returns the hub's answer, or None for any other update."""
    cq = (update or {}).get("callback_query") or {}
    data = str(cq.get("data") or "")
    uid = int(((cq.get("from") or {}).get("id")) or 0)
    if data[:2] not in ("a:", "d:") or uid <= 0:
        return None
    return hub.on_owner_decision(data[2:], uid, approved=data.startswith("a:"))
