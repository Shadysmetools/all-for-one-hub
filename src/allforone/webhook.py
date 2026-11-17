# SPDX-License-Identifier: Apache-2.0
"""RECEIVE (stage 1): the WhatsApp webhook edge.

``parse_all`` turns a Meta Cloud-API webhook payload into EVERY inbound message it carries (all entries, all
changes, all messages; text, voice and image/video/document). ``make_server`` exposes it over a stdlib
``http.server`` on loopback (a reverse proxy or tunnel connects to localhost, so the bind is NOT a trust boundary):

  - POST: bad Content-Length -> 400, over ``max_body`` -> 413 (before reading), then the ``X-Hub-Signature-256``
    HMAC-SHA256 of the RAW body with the app secret (``hmac.compare_digest``) -> 401, and only THEN json parse.
  - An accepted POST is queued for ONE worker thread and answered with an EMPTY 200 (nothing about the pipeline
    result leaks to the caller; Meta only needs the 200). The result is logged server-side, redacted.
  - GET does Meta's verification handshake; the verify token is REQUIRED (no default) and compared in constant time.
"""
from __future__ import annotations

import hashlib
import hmac
import json
import logging
import threading
from collections import deque
from concurrent.futures import ThreadPoolExecutor
from datetime import UTC, datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

from .model import Inbound, detect_lang

log = logging.getLogger("allforone.webhook")

_MEDIA_TYPES = ("image", "video", "document")


# ---------------------------------------------------------------- parsing

def parse_all(payload: dict, phone_number_id: str | None = None) -> list[Inbound]:
    """Every inbound message in a Meta webhook payload. ``phone_number_id`` None = no filter; otherwise only
    changes addressed to that number are read (an empty string matches nothing: fail closed)."""
    out: list[Inbound] = []
    for entry in _as_list((payload or {}).get("entry")):
        for change in _as_list((entry or {}).get("changes")):
            value = (change or {}).get("value") or {}
            if phone_number_id is not None and \
                    str((value.get("metadata") or {}).get("phone_number_id") or "") != str(phone_number_id):
                continue
            out.extend(_change_messages(value))
    return out


def parse(payload: dict) -> Inbound | None:
    """First inbound message, or None (status callback / nothing parsable). Kept for single-message callers."""
    got = parse_all(payload)
    return got[0] if got else None


def _as_list(x) -> list:
    return x if isinstance(x, list) else []


def _change_messages(value: dict) -> list[Inbound]:
    contacts = {str((c or {}).get("wa_id") or ""): str(((c or {}).get("profile") or {}).get("name") or "")
                for c in _as_list(value.get("contacts"))}
    out = []
    for msg in _as_list(value.get("messages")):
        try:
            inb = _one(msg, contacts)
        except (AttributeError, TypeError, ValueError):
            inb = None
        if inb is not None:
            out.append(inb)
    return out


def _one(msg: dict, contacts: dict[str, str]) -> Inbound | None:
    wa_id = str(msg.get("from") or "")
    base = dict(message_id=str(msg.get("id") or ""), wa_id=wa_id, name=contacts.get(wa_id, ""),
                ts=_ts(msg.get("timestamp")))
    mtype = msg.get("type")
    if mtype == "text":
        body = str((msg.get("text") or {}).get("body") or "")
        return Inbound(kind="text", text=body, lang=detect_lang(body), **base)
    if mtype in ("audio", "voice"):
        audio = msg.get("audio") or msg.get("voice") or {}
        return Inbound(kind="voice", audio_id=str(audio.get("id") or ""), **base)
    if mtype in _MEDIA_TYPES:
        m = msg.get(mtype) or {}
        cap = str(m.get("caption") or "")
        return Inbound(kind="media", text=cap or None, lang=detect_lang(cap), media_id=str(m.get("id") or ""),
                       media_type=mtype, mime=str(m.get("mime_type") or "") or None, **base)
    return None                                     # reaction/location/etc.: not a support message


def _ts(raw) -> datetime:
    try:
        return datetime.fromtimestamp(int(raw), tz=UTC)
    except (TypeError, ValueError, OverflowError, OSError):
        return datetime.now(UTC)


# ---------------------------------------------------------------- server

def _safe_summary(result) -> str:
    """A server-side log line: ids/stages only, then run through the shared redactor (no phone/name/token)."""
    from .redact import redact
    keep = {}
    if isinstance(result, dict):
        for k in ("ticket_id", "stage", "held", "ignored", "duplicate", "error", "resolved", "reopened"):
            if k in result:
                keep[k] = result[k]
        if "batch" in result:
            keep["batch"] = [_summary_dict(r) for r in result["batch"]]
    return redact(json.dumps(keep, default=str))


def _summary_dict(r) -> dict:
    return {k: r[k] for k in ("ticket_id", "stage", "held", "ignored", "duplicate", "error") if
            isinstance(r, dict) and k in r}


class HubServer(ThreadingHTTPServer):
    """Loopback server. The POST only persists + answers; processing runs on a SMALL WORKER POOL inside this one
    process (the hub's per-sender locks keep one customer's messages in order, so a slow PROVE-IT for one customer
    never queues the others behind it), and a timer thread runs ``hub.tick()`` (retries, backoff, approval expiry)
    in the SAME process, so the one-ticket / one-ack guarantees hold. ``drain()`` waits for queued work."""

    daemon_threads = True

    def __init__(self, addr, handler, hub, *, workers: int = 4, tick_interval: float | None = 60.0):
        super().__init__(addr, handler)
        self.hub = hub
        self._pool = ThreadPoolExecutor(max_workers=max(1, int(workers)), thread_name_prefix="hub-worker")
        self._pending = 0
        self._cv = threading.Condition()
        self._queues: dict[str, deque] = {}           # one serial queue per sender: at most ONE in-flight job each
        self._busy: set[str] = set()
        self._stop = threading.Event()
        self._ticker = None
        if tick_interval:
            self._ticker = threading.Thread(target=self._tick_loop, args=(float(tick_interval),), daemon=True)
            self._ticker.start()

    @staticmethod
    def _sender(payload: dict) -> str:
        try:
            return str(payload["entry"][0]["changes"][0]["value"]["messages"][0]["from"])
        except (KeyError, IndexError, TypeError):
            return ""

    def submit(self, payload: dict) -> None:
        key = self._sender(payload)
        with self._cv:
            self._pending += 1
            self._queues.setdefault(key, deque()).append(payload)
            if key in self._busy:                     # the sender already has a worker: wait in ITS queue, not on a worker
                return
            self._busy.add(key)
        self._pool.submit(self._drain_sender, key)

    def _drain_sender(self, key: str) -> None:
        while True:
            with self._cv:
                q = self._queues.get(key)
                if not q:
                    self._busy.discard(key)
                    self._queues.pop(key, None)
                    return
                payload = q.popleft()
            self._run(payload)

    def _run(self, payload: dict) -> None:
        try:
            result = self.hub.handle_webhook(payload)
            log.info("webhook processed: %s", _safe_summary(result))
        except Exception as e:                      # noqa: BLE001 - never let one payload kill a worker
            log.error("webhook processing failed: %s", type(e).__name__)
        finally:
            with self._cv:
                self._pending -= 1
                self._cv.notify_all()

    def _tick_loop(self, interval: float) -> None:
        while not self._stop.wait(interval):
            try:
                self.hub.tick()
            except Exception as e:                  # noqa: BLE001
                log.error("tick failed: %s", type(e).__name__)

    def drain(self, timeout: float = 15.0) -> None:
        with self._cv:
            self._cv.wait_for(lambda: self._pending == 0, timeout)

    def shutdown(self) -> None:
        super().shutdown()
        self._stop.set()
        self._pool.shutdown(wait=False)


def _require(name: str, value) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{name} is required (no default); read it from the vault")
    return value


def _sig_ok(app_secret: str, raw: bytes, header: str) -> bool:
    if not header.startswith("sha256="):
        return False
    want = hmac.new(app_secret.encode(), raw, hashlib.sha256).hexdigest()
    return hmac.compare_digest(want.encode(), header[len("sha256="):].strip().encode())


def make_server(hub, *, verify_token: str, app_secret: str, host: str = "127.0.0.1", port: int = 0,
                max_body: int = 1_048_576, workers: int = 4, tick_interval: float | None = 60.0) -> HubServer:
    """A loopback webhook server bound to ``hub``. Use ``server.server_address`` for the chosen port."""
    verify_token = _require("verify_token", verify_token)
    app_secret = _require("app_secret", app_secret)

    class Handler(BaseHTTPRequestHandler):
        hub_ = hub
        timeout = 10                                 # a short/stalled body can't pin a thread forever

        def log_message(self, *a):                  # keep tests quiet
            return

        def do_GET(self):                            # Meta verification handshake
            q = parse_qs(urlsplit(self.path).query)
            mode = (q.get("hub.mode") or [""])[0]
            token = (q.get("hub.verify_token") or [""])[0]
            challenge = (q.get("hub.challenge") or [""])[0]
            if mode == "subscribe" and hmac.compare_digest(token.encode(), verify_token.encode()):
                self._send(200, challenge.encode(), "text/plain")
            else:
                self._send(403, b"forbidden", "text/plain")

        def _length(self) -> int | None:
            raw = (self.headers.get("Content-Length") or "").strip()
            if not (raw.isascii() and raw.isdigit() and len(raw) < 10):
                return None
            return int(raw)

        def do_POST(self):
            length = self._length()
            if length is None:
                self._send(400, b"")
                return
            if length > max_body:
                self._send(413, b"")
                self.close_connection = True
                return
            raw = self.rfile.read(length)
            if not _sig_ok(app_secret, raw, self.headers.get("X-Hub-Signature-256") or ""):
                self._send(401, b"")
                return
            try:
                payload = json.loads(raw.decode("utf-8") or "{}")
            except ValueError:
                self._send(400, b"")
                return
            try:
                self.hub_.accept(payload)                 # durable BEFORE the 200: Meta does not resend after a 200
            except Exception as e:                  # noqa: BLE001 - nothing persisted -> let Meta retry
                log.error("could not persist webhook: %s", type(e).__name__)
                self._send(500, b"")
                return
            self.server.submit(payload)
            self._send(200, b"")                    # empty: the pipeline result is never returned to the caller

        def _send(self, code: int, body: bytes, ctype: str = "text/plain"):
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    return HubServer((host, port), Handler, hub, workers=workers, tick_interval=tick_interval)
