# SPDX-License-Identifier: Apache-2.0
"""Golden-set eval gate + independent judge. Acceptance tests."""
from __future__ import annotations

from pathlib import Path

from allforone import judge as judge_mod
from allforone.evalharness import DefaultEvalHarness
from allforone.model import Analysis
from hub_helpers import build_hub, deliver, run_to_resolved

FIX = Path(__file__).parent.parent / "src" / "allforone" / "fixtures"

DRAFTER_MODEL = "claude-drafter"   # production drafter; the judge must be a DIFFERENT model


# ---------------------------------------------------------------- eval harness gate

def test_16a_harness_scores_golden_set_and_passes():
    res = DefaultEvalHarness().run_gate()
    assert res["gate_pass"] is True and res["score"] >= res["pass_threshold"]


def test_16a_injected_regression_fails_the_gate():
    good = DefaultEvalHarness().run()                         # baseline (clean classifier)

    def worse(text, client_context=None):                    # a deliberately degraded classifier
        a = Analysis(nature="issue", ticket_type="Inquiries", track="inquiry", module="unknown", summary=text)
        return a

    bad = DefaultEvalHarness(classifier=worse)
    gate = bad.run_gate(baseline=good)
    assert gate["regressed"] is True and gate["gate_pass"] is False


# ---------------------------------------------------------------- independent judge

def test_16b_judge_flags_bad_draft_passes_good_one():
    j = judge_mod.FakeJudge()
    good = j.score("draft", {"text": "Hello Jordan, your ticket #7 is resolved. — Northwind Supplies Support", "expect_lang": "en"})
    bad = j.score("draft", {"text": "obviously your fault, guaranteed 100% forever", "expect_lang": "en"})
    assert good["flag"] is False and set(good["scores"]) == set(judge_mod.DIMENSIONS)
    assert bad["flag"] is True


def test_16b_judge_is_a_distinct_model_from_the_drafter():
    assert judge_mod.FakeJudge().model != DRAFTER_MODEL


def test_16b_low_score_draft_routes_to_owner_review():
    # a judge that flags everything -> the confirm draft is marked owner-review (and still needs approval)
    class FlagAll(judge_mod.FakeJudge):
        def score(self, kind, item):
            r = super().score(kind, item)
            r["flag"] = True
            return r

    hub = build_hub(judge=FlagAll())
    deliver(hub, "webhook_text.json")
    drafts = [p["text"] for p in hub.telegram.posts if "confirm draft" in p["text"]]
    assert drafts and "LOW SCORE: owner-review" in drafts[0]
    assert hub.whatsapp.automatic_count() == 1 and len(hub.whatsapp.sent) == 1     # still needs the owner's approval


# ---------------------------------------------------------------- feedback + agreement metric

def test_16c_judge_scores_feed_learning_and_kpi():
    hub = build_hub()
    run_to_resolved(hub)
    assert "video" in hub.learning.records[0]["judge_scores"]       # learning signal
    assert "judge_video_score" in hub.kpi.pushed[0]["outcome"]   # stage-11 KPI value


def test_16c_agreement_metric():
    pairs = [(True, True), (False, False), (True, False), (True, True)]   # 3/4 agree
    assert judge_mod.agreement(pairs) == 0.75
    assert judge_mod.agreement([]) == 0.0


# ---------------------------------------------------------------- concrete config + redaction

def test_16e_redaction_strips_name_and_phone_before_judge():
    red = judge_mod.redact_for_judge("Hello Jordan, call me on +15550100099 about ticket #7", names=["Jordan"])
    assert "Jordan" not in red and "+15550100099" not in red and "[NAME]" in red


def test_16e_llm_judge_parses_scores_and_redacts_before_the_call():
    seen = []

    def complete(prompt):
        seen.append(prompt)
        return 'Sure: {"correctness": 0.9, "tone": 0.8, "language": 1, "policy": 0.7, "completeness": 0.9}'

    j = judge_mod.LLMJudge(complete, model="primary-model")
    out = j.score("draft", {"text": "Hi Jordan +15550100099"}, names=["Jordan"])
    assert out["aggregate"] == 0.86 and out["flag"] is False and out["redacted"] is True
    assert "Jordan" not in seen[0] and "15550100099" not in seen[0]


def test_16e_llm_judge_blocks_on_a_bad_answer_instead_of_inventing_a_score():
    out = judge_mod.LLMJudge(lambda p: "no json here").score("draft", {"text": "x"})
    assert out["status"] == "blocked" and "aggregate" not in out


def test_16e_real_judge_blocks_without_a_model_but_redacts():
    j = judge_mod.resolve_judge()
    out = j.score("draft", {"text": "Hi Jordan +15550100099"}, names=["Jordan"])
    assert out["status"] == "blocked" and out["redacted"] is True and out["via"] == "fallback"


def test_16e_primary_fallback_routing():
    class Stub:
        def __init__(self, blocked, model):
            self.blocked, self.model = blocked, model

        def score(self, kind, item, names=None):
            if self.blocked:
                return {"status": "blocked", "model": self.model}
            return {"status": "ok", "model": self.model, "aggregate": 0.9}

    # primary works -> used
    p = judge_mod.PrimaryFallbackJudge(Stub(False, "primary-model"), Stub(False, "fallback-model"))
    assert p.score("draft", {"text": "x"})["via"] == "primary"
    # primary blocked -> fallback
    f = judge_mod.PrimaryFallbackJudge(Stub(True, "primary-model"), Stub(False, "fallback-model"))
    out = f.score("draft", {"text": "x"})
    assert out["via"] == "fallback" and out["model"] == "fallback-model"


def test_16e_assignment_is_not_sent_to_the_judge_in_pipeline():
    hub = build_hub()
    deliver(hub, "webhook_text.json")
    kinds = {s["kind"] for s in hub.judge.scored}
    assert "assignment" not in kinds and "classification" in kinds and "draft" in kinds
