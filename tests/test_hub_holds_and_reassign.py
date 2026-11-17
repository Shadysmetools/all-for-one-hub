# SPDX-License-Identifier: Apache-2.0
"""Security re-review of 08b7d81: reassign/release_hold are actor-gated, no silent MemoryStore, accept() never waits on
the processing lock, the draft judge never sees the client name/link, SQLite files are 0600."""
from __future__ import annotations

import http.client
import os
import threading

import pytest

from allforone import fakes
from allforone.model import Owner
from allforone.store import SqliteStore
from hub_helpers import approve, build_hub, deliver, load, pending
from test_hub_webhook_security import serve

PHONE = "15550100001"
INTRUDER = Owner(uid=999, name="Mallory", tg_id=6666)


def test_non_owner_cannot_reassign_to_themselves_and_approve():
    hub = build_hub()
    deliver(hub, "webhook_text.json")
    tid = hub.latest_ticket(PHONE).tid
    out = hub.reassign(tid, INTRUDER, actor_tg_id=6666)             # a group member, not the owner/allowlist
    assert out["ok"] is False and out["reason"] == "not_authorised"
    assert hub.ticket(tid).owner.name == "Maya" and 6666 not in pending(hub, "confirm")["allowed_approvers"]
    assert approve(hub, "confirm", as_tg=6666)["ok"] is False and len(hub.whatsapp.sent) == 1


@pytest.mark.parametrize("actor", [None, 0, "x"])
def test_reassign_fails_closed_without_a_valid_actor(actor):
    hub = build_hub()
    deliver(hub, "webhook_text.json")
    assert hub.reassign(PHONE, INTRUDER, actor_tg_id=actor)["ok"] is False
    assert hub.latest_ticket(PHONE).owner.name == "Maya"


def test_owner_and_allowlisted_actor_can_reassign():
    hub = build_hub(cfg={"phone_number_id": "PHONE_NUMBER_ID", "approver_tg_ids": [4242]})
    deliver(hub, "webhook_text.json")
    assert hub.reassign(PHONE, Owner(uid=777, name="Alex", tg_id=1001), actor_tg_id=1003)["to"] == "Alex"
    assert hub.reassign(PHONE, Owner(uid=778, name="Maya", tg_id=1003), actor_tg_id=4242)["to"] == "Maya"


def test_release_hold_is_actor_gated():
    hub = build_hub(knowledge=fakes.FakeKnowledge(partners={}))
    deliver(hub, "webhook_text.json")
    hub.k_primary.partners[PHONE] = 3001
    hid = hub.holds()[0]["id"]
    assert hub.release_hold(hid, actor_tg_id=None)["released"] is False
    assert hub.release_hold(hid, actor_tg_id=6666)["released"] is False and len(hub.holds()) == 1


def test_release_hold_by_allowlisted_actor_works():
    hub = build_hub(knowledge=fakes.FakeKnowledge(partners={}),
                    cfg={"phone_number_id": "PHONE_NUMBER_ID", "approver_tg_ids": [4242]})
    deliver(hub, "webhook_text.json")
    hub.k_primary.partners[PHONE] = 3001
    assert hub.release_hold(hub.holds()[0]["id"], actor_tg_id=4242).get("ticket_id")


def test_hub_requires_an_explicit_store_unless_run_mode_is_test():
    from allforone.pipeline import Hub
    from allforone.store import MemoryStore
    base = build_hub()
    seams = {k: getattr(base, a) for k, a in (("helpdesk", "helpdesk"), ("whatsapp", "whatsapp"), ("stt", "stt"),
             ("telegram", "telegram"), ("knowledge", "k_primary"), ("knowledge_notes", "k_notes"),
             ("assigner", "assigner"), ("proof", "proof"), ("kpi_sink", "kpi"), ("judge", "judge"),
             ("learning", "learning"))}
    with pytest.raises(ValueError):
        Hub(**seams, cfg={"run_mode": "live"})
    with pytest.raises(ValueError):
        Hub(**seams)
    assert Hub(**seams, cfg={"run_mode": "test"}).store is not None
    assert Hub(**seams, cfg={"run_mode": "live"}, store=MemoryStore()).store is not None


def test_accept_does_not_wait_for_the_processing_lock():
    hub = build_hub()
    done = threading.Event()
    with hub._wa_lock(PHONE):                                                   # a slow STT / PROVE-IT holds the hub lock
        t = threading.Thread(target=lambda: (hub.accept(load("webhook_text.json")), done.set()))
        t.start()
        assert done.wait(2), "accept() blocked behind the processing lock"
    t.join()


def test_draft_judge_never_sees_the_client_name_or_share_link():
    seen = []

    class Spy(fakes.FakeKnowledge):
        pass

    from allforone import judge as judge_mod

    class SpyJudge(judge_mod.FakeJudge):
        def score(self, kind, item):
            seen.append((kind, str(item)))
            return super().score(kind, item)

    ctx = {3001: {"name": "Contoso Retail", "modules": [], "owner_uids": [778]}}
    hub = build_hub(judge=SpyJudge(), knowledge=Spy(context=ctx))
    deliver(hub, "webhook_text.json")
    approve(hub, "confirm")
    deliver(hub, "webhook_confirm.json")
    approve(hub, "advance")
    approve(hub, "fix_ready")
    drafts = [s for k, s in seen if k == "draft"]
    assert drafts and all("Acme" not in s and "proof.example.com" not in s for s in drafts), drafts


def test_sqlite_files_are_chmodded_0600(tmp_path, monkeypatch):
    calls = []
    real = os.chmod
    monkeypatch.setattr(os, "chmod", lambda p, m, *a, **k: (calls.append((str(p), m)), real(p, m))[1])
    s = SqliteStore(str(tmp_path / "hub.db"))
    s.put("t", "1", {"a": 1})
    s.close()
    names = {os.path.basename(p) for p, m in calls if m == 0o600}
    assert {"hub.db", "hub.db-wal", "hub.db-shm"} <= names


def test_content_length_edge_cases_are_400_not_a_dead_thread():
    with serve(build_hub()) as (srv, base):
        host, port = srv.server_address
        for bad in ("²", "9" * 40):
            c = http.client.HTTPConnection(host, port, timeout=10)
            c.putrequest("POST", "/webhook")
            c.putheader("Content-Length", bad)
            c.endheaders()
            assert c.getresponse().status in (400, 413)
            c.close()


def test_handler_has_a_socket_timeout():
    with serve(build_hub()) as (srv, base):
        assert srv.RequestHandlerClass.timeout == 10
