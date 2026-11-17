# SPDX-License-Identifier: Apache-2.0
"""PROVE-IT (stage 9): produce evidence that a fix works, e.g. a short proof video with a share link.

The ``ProofRunner`` protocol (``interfaces.py``) is the seam; plug in whatever drives your capture pipeline.
``FakeProofRunner`` returns a deterministic ``prepared`` artifact with an unmistakably fake share link
(``proof.example.com``), used by the tests and the demo. ``CommandProofRunner`` is a minimal real runner that
shells out to a command you configure and expects a JSON artifact on stdout; it blocks (never fabricates) when no
command is configured.
"""
from __future__ import annotations

import json
import shlex
import subprocess


class FakeProofRunner:
    """Canned proof artifact. ``verdict`` drives the resolve path; the fake link is clearly simulated."""

    def __init__(self, *, verdict: str = "reproduced"):
        self.verdict = verdict
        self.calls: list[dict] = []

    def prove(self, request: dict, packet: dict, cfg: dict) -> dict:
        rid = str(request.get("id") or packet.get("request_id") or "REQ-0000")
        client = str(request.get("company") or packet.get("company") or "UNKNOWN")
        ref = str(request.get("ticket_ref") or rid)
        self.calls.append({"rid": rid, "client": client})
        return {"status": "prepared", "route": "proof_pair", "verdict": self.verdict,
                "share_link": f"https://proof.example.com/s/{client}/{ref}",
                "video_path": f"data/proof/{rid}/proof.mp4", "simulated": True,
                "note": "proof capture simulated (fake runner)"}


class CommandProofRunner:
    """Runs ``cmd`` (from ``cfg['proof']['command']``) with the request as JSON on stdin; the command prints a JSON
    artifact (``status``, ``verdict``, ``share_link`` ...). Anything else is reported as ``blocked``."""

    def __init__(self, command: str = "", *, timeout: float = 600):
        self.command, self.timeout = command, timeout

    def prove(self, request: dict, packet: dict, cfg: dict) -> dict:
        command = self.command or str(((cfg or {}).get("proof") or {}).get("command") or "")
        if not command:
            return {"status": "blocked", "why": "no proof command configured (proof.command)"}
        try:
            out = subprocess.run(shlex.split(command), input=json.dumps({"request": request, "packet": packet}),
                                 capture_output=True, text=True, timeout=self.timeout, check=True)
            art = json.loads(out.stdout)
        except (OSError, subprocess.SubprocessError, ValueError) as e:
            return {"status": "blocked", "why": f"proof command failed: {type(e).__name__}"}
        return art if isinstance(art, dict) else {"status": "blocked", "why": "proof command returned no object"}
