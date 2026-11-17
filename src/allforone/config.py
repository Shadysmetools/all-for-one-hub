# SPDX-License-Identifier: Apache-2.0
"""Configuration loading. One YAML file drives the team, routing weights, module keywords, templates (EN + AR),
helpdesk team and stage ids, and the safety gates. Secrets never live in it: pass tokens through the environment
or your secret store (see ``config.example.yaml``)."""
from __future__ import annotations

from importlib import resources
from pathlib import Path

import yaml


def load_config(path: str | Path) -> dict:
    data = yaml.safe_load(Path(path).read_text(encoding="utf-8")) or {}
    if not isinstance(data, dict):
        raise ValueError("config must be a YAML mapping")
    return data


def demo_config() -> dict:
    """The packaged demo configuration (fictional company, demo agents)."""
    text = resources.files("allforone").joinpath("data/demo_config.yaml").read_text(encoding="utf-8")
    return yaml.safe_load(text)
