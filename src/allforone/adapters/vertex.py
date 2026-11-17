# SPDX-License-Identifier: Apache-2.0
"""Vertex AI text completion for ``judge.LLMJudge`` (extra: ``pip install allforone[vertex]``).

``vertex_complete(model=...)`` returns a ``complete(prompt) -> str`` callable, or ``None`` when the SDK or the
project is missing (the judge then reports ``blocked``). Credentials come from Application Default Credentials.
"""
from __future__ import annotations

import os
from typing import Callable


def vertex_complete(*, model: str, project: str = "", location: str = "us-central1") -> Callable[[str], str] | None:
    project = project or os.environ.get("VERTEX_PROJECT", "")
    if not project:
        return None
    try:
        from google import genai
    except ImportError:
        return None
    client = genai.Client(vertexai=True, project=project, location=location)

    def complete(prompt: str) -> str:
        return str(client.models.generate_content(model=model, contents=prompt).text or "")

    return complete
