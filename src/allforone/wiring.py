# SPDX-License-Identifier: Apache-2.0
"""Build a fully-faked ``Hub`` from a config dict. Used by the demo and the ``tick`` diagnostic. Every seam can be
overridden by keyword, so the same function wires a half-real hub (e.g. a real STT with everything else faked)."""
from __future__ import annotations

from datetime import datetime

from . import fakes, judge, learning, proof_runner, routing
from .helpdesk.fake import FakeHelpdesk
from .helpdesk.gateway import GuardedHelpdesk
from .kpi import FakeKpiSink
from .pipeline import Hub
from .store import MemoryStore
from .templates import Templates


def build_fake_hub(cfg: dict, *, clock=None, store=None, **over) -> Hub:
    transport = over.pop("transport", None) or FakeHelpdesk()
    templates = Templates.from_config(cfg)
    team = routing.team_from_config(cfg) or routing.default_team()
    seams = dict(
        helpdesk=GuardedHelpdesk(transport, writes_on=True, cfg=cfg),
        whatsapp=fakes.FakeWhatsApp(),
        stt=fakes.FakeSTT(),
        telegram=fakes.FakeTelegram(),
        knowledge=fakes.FakeKnowledge(),
        knowledge_notes=fakes.FakeNotesKnowledge(),
        assigner=routing.RuleAssigner(team, weights=(cfg.get("routing") or {}).get("weights")),
        proof=proof_runner.FakeProofRunner(),
        kpi_sink=FakeKpiSink(),
        judge=judge.FakeJudge(sign_markers=templates.sign_markers(),
                              threshold=float((cfg.get("judge") or {}).get("threshold", judge.DEFAULT_THRESHOLD))),
        learning=learning.FakeLearningLog(),
        cfg=cfg,
        clock=clock or (lambda: datetime.now().astimezone()),
        store=store if store is not None else MemoryStore(),
    )
    seams.update(over)
    return Hub(**seams)
