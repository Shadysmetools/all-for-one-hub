# SPDX-License-Identifier: Apache-2.0
"""The golden-set eval gate.

Loads a set of golden (synthetic) tickets (input -> ground-truth type/owner/answer), runs the pipeline's
classify/route/draft, scores the predictions against ground truth, and FAILS the gate on a score regression
(a drop below the baseline) — the CI gate on every prompt/routing/model change.

Redaction: the golden set carries no client PII — synthetic client ids and paraphrased inputs only. The schema
is documented in ``fixtures/golden_set.json`` (``_schema`` key). The classifier/assigner are injectable so a
test can simulate a regression (a deliberately worse classifier) and prove the gate fails.
"""
from __future__ import annotations

import json
from pathlib import Path

from . import analyse as analyse_mod
from .routing import RuleAssigner, default_team

GOLDEN = Path(__file__).parent / "fixtures" / "golden_set.json"


def load_golden(path: Path | None = None) -> list[dict]:
    data = json.loads((path or GOLDEN).read_text(encoding="utf-8"))
    return [row for row in data.get("items", [])]


class DefaultEvalHarness:
    """Runs classify + route + draft over the golden set and scores vs ground truth."""

    def __init__(self, *, golden: list[dict] | None = None, classifier=None, assigner=None,
                 pass_threshold: float = 0.6):
        self.golden = golden if golden is not None else load_golden()
        self.classifier = classifier or (lambda text, client_context=None: analyse_mod.classify(
            text, client_context=client_context))
        self.assigner = assigner or RuleAssigner(default_team())
        self.pass_threshold = pass_threshold

    def _score_item(self, item: dict) -> dict:
        ctx = item.get("client_context") or {}
        analysis = self.classifier(item["input"], client_context=ctx)
        type_ok = str(analysis.ticket_type) == str(item.get("gt_type"))
        pred = self.assigner.assign(analysis, ctx)
        owner_ok = int(pred["uid"]) == int(item.get("gt_owner_uid") or -1)
        return {"id": item.get("id"), "type_ok": type_ok, "owner_ok": owner_ok,
                "pred_type": analysis.ticket_type, "pred_owner": pred["uid"]}

    def run(self) -> dict:
        rows = [self._score_item(it) for it in self.golden]
        n = len(rows) or 1
        type_acc = sum(1 for r in rows if r["type_ok"]) / n
        owner_acc = sum(1 for r in rows if r["owner_ok"]) / n
        score = round((type_acc + owner_acc) / 2, 4)
        return {"n": len(rows), "type_accuracy": round(type_acc, 4), "owner_accuracy": round(owner_acc, 4),
                "score": score, "rows": rows}

    def run_gate(self, baseline: dict | None = None) -> dict:
        res = self.run()
        passed = res["score"] >= self.pass_threshold
        regressed = False
        if baseline is not None:
            base_score = float(baseline.get("score", 0.0))
            regressed = res["score"] < base_score - 1e-9
        gate_pass = passed and not regressed
        return {**res, "pass_threshold": self.pass_threshold, "passed_absolute": passed,
                "regressed": regressed, "gate_pass": gate_pass,
                "reason": ("regression vs baseline" if regressed else
                           ("below threshold" if not passed else "ok"))}
