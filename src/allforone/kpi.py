# SPDX-License-Identifier: Apache-2.0
"""KPI sinks: where a resolved ticket's outcome goes. The hub only needs ``push_kpis(outcome) -> {"ok": bool}``."""
from __future__ import annotations

import json
import sys
from collections import defaultdict
from pathlib import Path
from typing import TextIO


class FakeKpiSink:
    """Records the KPI payload that WOULD be pushed. No file, no network."""

    def __init__(self):
        self.pushed: list[dict] = []

    def push_kpis(self, outcome: dict) -> dict:
        rec = {"ok": True, "simulated": True, "outcome": dict(outcome)}
        self.pushed.append(rec)
        return rec


class StdoutKpiSink:
    """One JSON line per resolved ticket on a text stream (stdout by default)."""

    def __init__(self, stream: TextIO | None = None):
        self.stream = stream

    def push_kpis(self, outcome: dict) -> dict:
        print(json.dumps(outcome, default=str, sort_keys=True), file=self.stream or sys.stdout)
        return {"ok": True}


class JsonlKpiSink:
    """Appends one JSON line per resolved ticket to a file."""

    def __init__(self, path: str | Path):
        self.path = Path(path)

    def push_kpis(self, outcome: dict) -> dict:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with self.path.open("a", encoding="utf-8") as f:
            f.write(json.dumps(outcome, default=str, sort_keys=True) + "\n")
        return {"ok": True}


def per_owner(outcomes: list[dict]) -> dict:
    """Aggregate resolved outcomes per owner: tickets, reopened count and mean judge score for the proof video."""
    acc: dict = defaultdict(lambda: {"resolved": 0, "reopened": 0, "scores": []})
    for o in outcomes:
        a = acc[o.get("owner_uid", 0)]
        a["resolved"] += 1
        a["reopened"] += 1 if o.get("reopen_count") else 0
        if o.get("judge_video_score") is not None:
            a["scores"].append(float(o["judge_video_score"]))
    return {uid: {"resolved": a["resolved"], "reopened": a["reopened"],
                  "mean_video_score": round(sum(a["scores"]) / len(a["scores"]), 3) if a["scores"] else None}
            for uid, a in acc.items()}
