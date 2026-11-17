# SPDX-License-Identifier: Apache-2.0
"""The demo runs the whole client loop on fakes, deterministically, in both languages."""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

from allforone import demo


def _run(lang, tmp_path):
    out = tmp_path / lang
    assert demo.main(["--lang", lang, "--out", str(out), "--no-color"]) == 0
    return json.loads((out / "timeline.json").read_text(encoding="utf-8"))


@pytest.mark.parametrize("lang", ["en", "ar"])
def test_demo_walks_all_13_steps_with_one_automatic_message(lang, tmp_path, capsys):
    doc = _run(lang, tmp_path)
    assert [e["step"] for e in doc["events"]] == list(range(1, 14))
    assert doc["events"][5]["payload"]["automatic"] is True
    assert doc["events"][-1]["payload"]["automatic_messages"] == 1
    assert doc["events"][11]["payload"]["stage"] == "resolved"
    assert all(e["payload"] for e in doc["events"]) and "[13/13]" in capsys.readouterr().out


def test_demo_timeline_is_deterministic(tmp_path):
    assert _run("en", tmp_path / "a") == _run("en", tmp_path / "b")


def test_demo_arabic_variant_speaks_arabic(tmp_path):
    doc = _run("ar", tmp_path)
    assert "المخزون" in doc["events"][1]["payload"]["transcript"]
    assert "دعم" in doc["events"][5]["payload"]["text"]


def test_demo_has_no_colour_codes_when_piped_and_runs_as_a_module(tmp_path):
    root = Path(__file__).parent.parent
    r = subprocess.run([sys.executable, "-m", "allforone.demo", "--out", str(tmp_path)], capture_output=True,
                       text=True, encoding="utf-8", cwd=root, check=True)
    assert "\x1b[" not in r.stdout and "[ 1/13]" in r.stdout
