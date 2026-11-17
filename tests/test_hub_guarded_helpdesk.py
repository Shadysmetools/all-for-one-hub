# SPDX-License-Identifier: Apache-2.0
"""GuardedHelpdesk must not turn helpdesk writes on by itself, and the mail-suppress context must be exact."""
from __future__ import annotations

import pytest

from allforone.config import demo_config
from allforone.helpdesk import guard
from allforone.helpdesk.fake import FakeHelpdesk
from allforone.helpdesk.gateway import MAIL_SUPPRESS, GuardedHelpdesk, mail_suppress_context

VALS = {"name": "x", "stage_id": 901, "team_id": 2, "partner_id": 3001}


class Spy(FakeHelpdesk):
    def __init__(self):
        super().__init__()
        self.calls = []

    def execute_kw(self, model, method, args, kwargs):
        self.calls.append((model, method))
        return super().execute_kw(model, method, args, kwargs)


def _writes(spy):
    return [c for c in spy.calls if c[1] in ("create", "write", "message_post")]


def test_default_is_writes_off_for_every_write_path():
    spy = Spy()
    gw = GuardedHelpdesk(spy)                                    # no writes_on argument
    assert gw.adapter.writes_on is False
    for call in (lambda: gw.create_ticket(dict(VALS), context=mail_suppress_context()),
                 lambda: gw.assign(1, 777), lambda: gw.post_note(1, "hi")):
        with pytest.raises(guard.WriteRefused):
            call()
    assert _writes(spy) == []                                # nothing reached the transport


def test_from_config_follows_writes_allowed_gate():
    live_on = {"run_mode": "live", "helpdesk": {"write_enabled": True}}
    assert GuardedHelpdesk.from_config(FakeHelpdesk(), live_on).adapter.writes_on is True
    for cfg in ({}, {"run_mode": "observe", "helpdesk": {"write_enabled": True}},
                {"run_mode": "live", "helpdesk": {"write_enabled": False}},
                {"run_mode": "live", "helpdesk": {"write_enabled": "yes"}}):
        assert GuardedHelpdesk.from_config(FakeHelpdesk(), cfg).adapter.writes_on is False, cfg


@pytest.mark.parametrize("ctx", [{**MAIL_SUPPRESS, "force_send": True}, {**MAIL_SUPPRESS, "tracking_disable": False},
                                 {}, {"tracking_disable": True}])
def test_create_context_must_equal_mail_suppress_exactly(ctx):
    spy = Spy()
    gw = GuardedHelpdesk(spy, writes_on=True, cfg=demo_config())
    with pytest.raises(guard.WriteRefused):
        gw.create_ticket(dict(VALS), context=ctx)
    assert _writes(spy) == []


def test_exact_context_is_accepted_and_reaches_the_transport():
    spy = Spy()
    gw = GuardedHelpdesk(spy, writes_on=True, cfg=demo_config())
    assert gw.create_ticket(dict(VALS), context=mail_suppress_context())
    assert ("helpdesk.ticket", "create") in spy.calls
