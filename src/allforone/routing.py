# SPDX-License-Identifier: Apache-2.0
"""ASSIGN (stage 5): team-aware assignment.

The router reads the knowledge sources (owner-by-client / owner-by-module / owner-by-type + current load) and
scores candidates; the top score wins. Every assignment carries an "assigned to X because..." rationale (the
factors and weights that decided it) for the owner's brief, and a one-tap override writes back a learning signal.

This is a deterministic assigner (no credentials needed): with the knowledge sources stubbed it still returns a
deterministic choice and rationale. The same interface can be backed by a model; the judge then scores the
assignment.
"""
from __future__ import annotations

from dataclasses import dataclass

from .model import Analysis, Owner

# Default scoring weights. Positive factors add; load subtracts (avoid piling onto a saturated owner).
WEIGHTS = {"owns_client": 3.0, "owns_module": 2.0, "owns_type": 1.5, "role_fit": 1.0, "load": 0.5}


@dataclass
class Candidate:
    """A seeded team member (role/skills/load). In production these come from team membership + open-ticket counts."""

    owner: Owner
    skills: tuple[str, ...] = ()         # modules/areas this person handles
    handles_tracks: tuple[str, ...] = ()  # inquiry/functional/development they cover
    open_load: int = 0                   # current open-ticket count (from the helpdesk)


def _role_fit(cand: Candidate, analysis: Analysis) -> float:
    if analysis.track in cand.handles_tracks:
        return 1.0
    return 0.0


def score_candidate(cand: Candidate, analysis: Analysis, client_context: dict,
                    weights: dict | None = None) -> tuple[float, list[str]]:
    """Score one candidate and return (score, factor-citing reasons)."""
    W = {**WEIGHTS, **(weights or {})}
    reasons: list[str] = []
    score = 0.0
    owns_client = int(cand.owner.uid) in {int(u) for u in (client_context.get("owner_uids") or [])}
    if owns_client:
        score += W["owns_client"]
        reasons.append("owns this client (history)")
    if analysis.module != "unknown" and analysis.module in cand.skills:
        score += W["owns_module"]
        reasons.append(f"handles the {analysis.module} module")
    type_owner = (client_context.get("type_owner") or {}).get(analysis.ticket_type)
    if type_owner and int(type_owner) == int(cand.owner.uid):
        score += W["owns_type"]
        reasons.append(f"usually handles {analysis.ticket_type} tickets")
    rf = _role_fit(cand, analysis)
    if rf:
        score += W["role_fit"] * rf
        reasons.append(f"role covers the {analysis.track} track")
    penalty = W["load"] * float(cand.open_load)
    score -= penalty
    if cand.open_load:
        reasons.append(f"current load {cand.open_load} open (−{penalty:g})")
    return round(score, 3), reasons


class RuleAssigner:
    """Deterministic assigner. Ties break by lower load, then lower uid (stable)."""

    def __init__(self, candidates: list[Candidate], *, fallback_uid: int = 0, weights: dict | None = None):
        self.candidates = list(candidates)
        self.weights = dict(weights or {})
        self.fallback_uid = fallback_uid

    def assign(self, analysis: Analysis, client_context: dict) -> dict:
        if not self.candidates:
            raise ValueError("no candidates to assign from")
        scored = []
        for c in self.candidates:
            s, reasons = score_candidate(c, analysis, client_context or {}, self.weights)
            scored.append((s, -c.open_load, -c.owner.uid, c, reasons))
        scored.sort(reverse=True)
        best = scored[0]
        cand, reasons = best[3], best[4]
        if not reasons:
            reasons = [f"fallback by track ({analysis.track})"]
        rationale = f"assigned to {cand.owner.name} because: " + "; ".join(reasons)
        runners = [{"name": s[3].owner.name, "uid": s[3].owner.uid, "score": s[0]} for s in scored]
        return {"owner": cand.owner, "uid": cand.owner.uid, "score": best[0],
                "rationale": rationale, "factors": reasons, "ranking": runners}


def default_team() -> list[Candidate]:
    """A seeded demo team (synthetic uids that match ``FakeHelpdesk``). Real deployments build the team from config
    with ``team_from_config``."""
    alex = Owner(uid=777, name="Alex", tg_id=1001, role="support")
    maya = Owner(uid=778, name="Maya", tg_id=1003, role="implementation-lead")
    return [
        Candidate(alex, skills=("Helpdesk", "Sales", "POS"), handles_tracks=("inquiry", "functional"), open_load=0),
        Candidate(maya, skills=("Inventory", "Manufacturing", "Accounting"),
                  handles_tracks=("development", "functional"), open_load=0),
    ]


def team_from_config(cfg: dict) -> list[Candidate]:
    """Build the candidates from ``cfg['team']``: a list of
    ``{uid, name, tg_id, role, skills: [...], tracks: [...]}``."""
    out = []
    for m in (cfg or {}).get("team") or []:
        owner = Owner(uid=int(m["uid"]), name=str(m["name"]), tg_id=int(m.get("tg_id") or 0),
                      role=str(m.get("role") or ""))
        out.append(Candidate(owner, skills=tuple(m.get("skills") or ()), handles_tracks=tuple(m.get("tracks") or ())))
    return out
