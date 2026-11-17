# SPDX-License-Identifier: Apache-2.0
"""The generic helpdesk guard and the read-only Odoo adapter."""
from __future__ import annotations

import pytest

from allforone.helpdesk import guard
from allforone.helpdesk.fake import FakeHelpdesk
from allforone.helpdesk.gateway import HelpdeskAdapter, team_id_for
from allforone.helpdesk.odoo_read import OdooKnowledge, OdooReadTransport, OdooUnavailable, open_readonly

SAFE = {901, 902, 903}


def _create(vals, **kw):
    guard.check("helpdesk", "helpdesk.ticket", "create", [vals], safe_stages=SAFE, **kw)


def test_create_needs_a_safe_stage_and_refuses_unlisted_fields():
    _create({"name": "x", "stage_id": 901, "partner_id": 1})
    with pytest.raises(guard.WriteRefused):
        _create({"name": "x", "partner_id": 1})                         # no stage: would land in "New"
    with pytest.raises(guard.WriteRefused):
        _create({"name": "x", "stage_id": 1, "partner_id": 1})          # a client-emailing stage
    with pytest.raises(guard.WriteRefused):
        _create({"name": "x", "stage_id": 901, "secret_field": 1})


def test_unlink_private_methods_and_ir_models_are_never_allowed():
    for model, method in (("helpdesk.ticket", "unlink"), ("helpdesk.ticket", "_write"), ("ir.attachment", "create"),
                          ("base.language.install", "lang_install")):
        with pytest.raises(guard.WriteRefused):
            guard.check("helpdesk", model, method, [[1]])


def test_write_cannot_change_client_or_team_and_notes_are_internal_only():
    with pytest.raises(guard.WriteRefused):
        guard.check("helpdesk", "helpdesk.ticket", "write", [[1], {"partner_id": 2}], safe_stages=SAFE)
    with pytest.raises(guard.WriteRefused):
        guard.check("helpdesk", "helpdesk.ticket", "write", [[1], {"team_id": 2}], safe_stages=SAFE)
    with pytest.raises(guard.WriteRefused):
        guard.check("helpdesk", "helpdesk.ticket", "message_post", [[1]], {"body": "hi", "subtype_xmlid": "mail.mt_comment"})
    guard.check("helpdesk", "helpdesk.ticket", "message_post", [[1]], {"body": "hi", "subtype_xmlid": "mail.mt_note"})


def test_policy_is_data_custom_and_forbidden_fields():
    policy = guard.DEFAULT_POLICY.with_custom_fields("x_region", forbidden=["x_locked"])
    _create({"name": "x", "stage_id": 901, "x_region": "north"}, policy=policy)
    with pytest.raises(guard.WriteRefused):
        _create({"name": "x", "stage_id": 901, "x_locked": 1}, policy=policy)
    with pytest.raises(guard.WriteRefused):
        _create({"name": "x", "stage_id": 901, "x_region": "north"})     # not on the default policy


def test_team_ids_come_from_config_and_fall_back_to_a_live_team():
    from allforone.helpdesk.gateway import Conventions
    conv = Conventions(teams={5: {}, 6: {}})
    assert team_id_for("inquiry", conv, {"default": 5, "inquiry": 6}) == 6
    assert team_id_for("inquiry", conv, {"default": 5, "inquiry": 99}) == 5
    assert team_id_for("functional", conv, {"default": 99}) in (5, 6)


def test_read_only_transport_refuses_every_write_at_the_transport():
    calls = []

    def http(url, **kw):
        calls.append(url)
        raise AssertionError("no request may be made for a write")

    t = OdooReadTransport("https://odoo.example.com", "db", "bot", "key", http=http)
    for method in ("create", "write", "unlink", "message_post"):
        with pytest.raises(guard.WriteRefused):
            t.execute_kw("helpdesk.ticket", method, [[1]], {})
    assert calls == []
    assert "key" not in repr(t)


def test_open_readonly_needs_config_and_never_enables_writes(monkeypatch):
    with pytest.raises(OdooUnavailable):
        open_readonly()
    a = open_readonly(url="https://odoo.example.com", db="db", login="bot", api_key="k")
    assert a.writes_on is False
    with pytest.raises(guard.WriteRefused):
        a.call("helpdesk", "helpdesk.ticket", "create", [{"name": "x", "stage_id": 901}], safe_stages=SAFE)


def test_odoo_knowledge_maps_a_number_to_a_partner_through_the_guard():
    fake = FakeHelpdesk()
    fake.data["res.partner"][3001]["mobile"] = "+15550100001"
    k = OdooKnowledge(HelpdeskAdapter(fake))
    assert k.partner_for("15550100001") == 3001
    assert k.partner_for("") is None
    assert k.client_context(3001)["owner_uids"] == [778]
