# SPDX-License-Identifier: Apache-2.0
"""All for One: a client-agnostic WhatsApp-to-helpdesk support hub with human-approved replies, an LLM judge and
self-learning.

A central HUB service owns WhatsApp I/O, helpdesk ticketing and the support pipeline. Every external system
(WhatsApp, speech-to-text, helpdesk, knowledge sources, Telegram approvals, proof runner, judge, KPI sink) sits
behind a small interface (``interfaces.py``) with a deterministic FAKE (``fakes.py``), so the whole pipeline runs in
CI and in the demo (``python -m allforone.demo``) with zero credentials and no client send.

Real helpdesk write-back to a live Odoo server is part of the commercial edition; this package ships the guard,
the write interface, a fake and a read-only Odoo adapter.

Pipeline stages (each has an acceptance test; ``Hub`` drives them, see ``pipeline.py``):
  1 RECEIVE  2 STT  3 ANALYSE  4 CREATE  5 ASSIGN  6 TELEGRAM  7 ACK  8 CONFIRM
  9 PROVE-IT 10 RESOLVE 11 KPIs   (+ learning and the team-aware assigner layered on ANALYSE / ASSIGN)
"""
from __future__ import annotations

from .model import Analysis, Inbound, Owner
from .pipeline import Hub, TicketState

__version__ = "0.1.0"
__all__ = ["Hub", "TicketState", "Analysis", "Inbound", "Owner"]
