# SPDX-License-Identifier: Apache-2.0
"""WhatsApp Cloud adapter, Groq STT, Telegram approvals, KPI sinks, config-driven templates, proof runner."""
from __future__ import annotations

import io
import json
from datetime import UTC, datetime, timedelta

import pytest

from allforone import kpi
from allforone.adapters.groq_stt import GroqSTT
from allforone.adapters.telegram_approvals import TelegramApprovals, TelegramError, handle_update
from allforone.channels import whatsapp_cloud as wa
from allforone.proof_runner import CommandProofRunner, FakeProofRunner
from allforone.templates import Templates

NOW = datetime(2026, 1, 12, 9, 0, tzinfo=UTC)
TO = "+1-555-0100"


def _cloud(sent, **kw):
    def http(method, url, *, headers, json=None, timeout=30):
        sent.append((method, url, json))
        return 200, {"messages": [{"id": "wamid.X"}]}
    base = dict(token="t0ken", phone_number_id="PID", graph_version="v1.0", allowed_recipients=[TO], enabled=True,
                http=http)
    return wa.WhatsAppCloud(**{**base, **kw})


def test_cloud_sends_inside_the_window_and_refuses_outside_or_to_strangers():
    sent = []
    c = _cloud(sent)
    assert c.send_text(TO, "hi", last_inbound_at=NOW - timedelta(hours=1), now=NOW)["msg_id"] == "wamid.X"
    with pytest.raises(wa.WindowClosed):
        c.send_text(TO, "hi", last_inbound_at=NOW - timedelta(hours=25), now=NOW)
    with pytest.raises(wa.WhatsAppRefused):
        c.send_text("+1-555-0199", "hi", last_inbound_at=NOW, now=NOW)
    assert len(sent) == 1 and "t0ken" not in repr(c)


def test_cloud_template_ignores_the_window_but_not_the_allowlist_or_kill_switch():
    sent = []
    c = _cloud(sent, kill_switch=lambda: False)
    c.send_template(TO, "order_update", "en", ["42"], now=NOW)
    assert sent[0][2]["template"]["components"][0]["parameters"][0]["text"] == "42"
    with pytest.raises(wa.WhatsAppRefused):
        _cloud(sent, kill_switch=lambda: True).send_template(TO, "x", "en", now=NOW)
    with pytest.raises(wa.WhatsAppRefused):
        _cloud(sent, enabled=False)


def test_cloud_errors_never_carry_the_recipient():
    def boom(*a, **k):
        raise ConnectionError(TO)
    with pytest.raises(wa.WhatsAppError) as e:
        _cloud([], http=boom).send_text(TO, "hi", last_inbound_at=NOW, now=NOW)
    assert "555" not in str(e.value)


class _Groq:
    class audio:                                                        # noqa: N801 - mimics the SDK shape
        class transcriptions:                                           # noqa: N801
            @staticmethod
            def create(**kw):
                return type("R", (), {"text": "  stock is wrong  "})()


def test_groq_stt_with_an_injected_client_needs_no_sdk_and_no_key():
    out = GroqSTT(lambda i: b"audio", client=_Groq()).transcribe("M1", lang_hint="en")
    assert out["text"] == "stock is wrong" and out["provider"] == "groq"


def test_groq_stt_blocks_without_a_key_and_when_the_fetch_fails():
    assert GroqSTT(lambda i: b"").transcribe("M1")["status"] == "blocked"

    def fetch(_):
        raise OSError("down")
    out = GroqSTT(fetch, client=_Groq()).transcribe("M1")
    assert out["status"] == "blocked" and "OSError" in out["why"]


def test_telegram_approvals_carry_only_the_nonce_and_route_taps_with_the_tapper_id():
    posts = []
    t = TelegramApprovals(token="123:abc", chat_id=-100, post=lambda url, payload: posts.append((url, payload)) or
                          {"message_thread_id": 7})
    assert t.open_topic(5, "Maya", "brief") == 7
    t.request_approval(7, ticket_id=5, kind="confirm", draft="D", nonce="N0NCE", draft_hash="h",
                       allowed_approvers=[1], expires_at=NOW)
    buttons = posts[-1][1]["reply_markup"]["inline_keyboard"][0]
    assert [b["callback_data"] for b in buttons] == ["a:N0NCE", "d:N0NCE"] and "123:abc" not in repr(t)

    class Hub:
        def on_owner_decision(self, nonce, uid, approved):
            return {"nonce": nonce, "uid": uid, "approved": approved}
    upd = {"callback_query": {"data": "d:N0NCE", "from": {"id": 1003}}}
    assert handle_update(Hub(), upd) == {"nonce": "N0NCE", "uid": 1003, "approved": False}
    assert handle_update(Hub(), {"message": {}}) is None
    assert handle_update(Hub(), {"callback_query": {"data": "a:N", "from": {}}}) is None


def test_telegram_failures_do_not_leak_the_token():
    def boom(url, payload):
        raise RuntimeError(url)
    with pytest.raises(TelegramError) as e:
        TelegramApprovals(token="123:abc", chat_id=1, post=boom).post_triage("x")
    assert "123:abc" not in str(e.value)


def test_kpi_sinks_write_json_lines_and_aggregate_per_owner(tmp_path):
    sink = kpi.JsonlKpiSink(tmp_path / "out" / "k.jsonl")
    sink.push_kpis({"ticket_id": 1, "owner_uid": 778, "judge_video_score": 0.9, "reopen_count": 0})
    sink.push_kpis({"ticket_id": 2, "owner_uid": 778, "judge_video_score": 0.7, "reopen_count": 1})
    rows = [json.loads(line) for line in (tmp_path / "out" / "k.jsonl").read_text().splitlines()]
    assert kpi.per_owner(rows) == {778: {"resolved": 2, "reopened": 1, "mean_video_score": 0.8}}
    buf = io.StringIO()
    assert kpi.StdoutKpiSink(buf).push_kpis({"a": 1})["ok"] and json.loads(buf.getvalue()) == {"a": 1}


def test_templates_are_overridable_from_config():
    t = Templates.from_config({"templates": {"sign": {"en": "-- Acme Help"}, "ack": {"en": "Hi {name} #{n} {owner} {next_step}"}},
                               "module_labels": {"ar": {"Billing": "الفواتير"}}})
    assert t.ack("en", name="Jo", n=1, owner="Al", next_step="x") == "Hi Jo #1 Al x -- Acme Help"
    assert t.sign("ar") == Templates().sign("ar")                       # untouched keys keep the defaults
    assert t.module_label("ar", "Billing") == "الفواتير"


def test_proof_runner_fake_is_deterministic_and_command_runner_blocks_unconfigured():
    art = FakeProofRunner().prove({"id": "R1", "company": "c", "ticket_ref": "9"}, {}, {})
    assert art["share_link"] == "https://proof.example.com/s/c/9" and art["simulated"] is True
    assert CommandProofRunner().prove({}, {}, {})["status"] == "blocked"
    ok = CommandProofRunner('python -c "print(1)"').prove({}, {}, {})
    assert ok["status"] == "blocked"                                    # stdout was not a JSON object
