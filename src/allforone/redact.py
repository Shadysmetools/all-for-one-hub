# SPDX-License-Identifier: Apache-2.0
"""Secrets and personal-data scanner.

Used before a draft reaches the judge, before an owner-edited text is sent and before anything is logged.
"""
from __future__ import annotations

import hashlib
import math
import re
from dataclasses import dataclass
from typing import Iterable

# Order matters: more specific patterns first. Each is (kind, compiled regex).
_PATTERNS: list[tuple[str, re.Pattern[str]]] = [
    ("private_key", re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----")),
    ("portal_token", re.compile(r"[?&]access_token=[A-Za-z0-9-]{8,}")),
    ("signup_link", re.compile(r"/web/(?:signup|reset_password)\?[^\s]*token=[A-Za-z0-9-_]+")),
    ("bearer", re.compile(r"(?i)\bbearer\s+[A-Za-z0-9\-._~+/]{16,}=*")),
    ("api_key", re.compile(r"\b(?:sk|pk|rk|xox[abp]|ghp|gho|github_pat|AKIA|AIza)[A-Za-z0-9_\-]{16,}")),
    ("api_key", re.compile(r"\b\d{8,10}:[A-Za-z0-9_-]{35}\b")),  # chat-bot token shape
    # 32 or 40 hex chars with letters and digits (API secrets). 64-hex audit hashes do not match.
    ("hex_key", re.compile(r"\b(?=[0-9a-fA-F]*[a-fA-F])(?=[0-9a-fA-F]*\d)(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{32})\b")),
    ("token_param", re.compile(r"(?i)\b(?:token|api_key|apikey|secret|session|auth)=[^\s&\"']{6,}")),
    ("password_kv", re.compile(r"(?i)\b(?:password|passwd|pwd|pass|كلمة السر|الباسورد)\s*[:=]\s*\S{4,}")),
    ("email", re.compile(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b")),
    # Phone numbers: generic +country numbers and local mobile shapes. Keeps record refs like IN/10001/26 safe.
    ("phone", re.compile(r"(?<![\w/])(?:\+|00)?(?:20|966)?\s?0?1[0125]\d[\s-]?\d{3,4}[\s-]?\d{4}(?![\w/])")),
    ("phone", re.compile(r"(?<![\w/])\+\d{1,3}[\s-]?\d{2,4}[\s-]?\d{3,4}[\s-]?\d{3,4}(?![\w/])")),
    ("phone", re.compile(r"(?<![\w/])05\d[\s-]?\d{3}[\s-]?\d{4}(?![\w/])")),
]

_LONG_TOKEN = re.compile(r"[A-Za-z0-9_\-+/=]{32,}")

# Emails are personal data, not secrets: has_secrets ignores them; the personal-data guard uses scan().
_NOT_SECRET = {"email", "phone"}


@dataclass
class Finding:
    kind: str
    start: int
    end: int


def value_hash(value: str) -> str:
    """sha256 hex of a secret value, so guards can spot a leaked value without storing it."""
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def _entropy(s: str) -> float:
    counts = {c: s.count(c) for c in set(s)}
    return -sum(n / len(s) * math.log2(n / len(s)) for n in counts.values())


def scan(text: str, vault_hashes: Iterable[str] = ()) -> list[Finding]:
    """Find secrets and personal data in text. Overlapping matches keep the first (most specific)."""
    if not text:
        return []
    found: list[Finding] = []

    def overlaps(a: int, b: int) -> bool:
        return any(f.start < b and a < f.end for f in found)

    for kind, rx in _PATTERNS:
        for m in rx.finditer(text):
            if not overlaps(m.start(), m.end()):
                found.append(Finding(kind, m.start(), m.end()))
    for m in _LONG_TOKEN.finditer(text):
        tok = m.group(0)
        if not overlaps(m.start(), m.end()) and _entropy(tok) > 4.0 and any(c.isdigit() for c in tok):
            found.append(Finding("high_entropy", m.start(), m.end()))
    hashes = set(vault_hashes)
    if hashes:
        for m in re.finditer(r"\S{6,}", text):
            if value_hash(m.group(0)) in hashes and not overlaps(m.start(), m.end()):
                found.append(Finding("vault_value", m.start(), m.end()))
    return sorted(found, key=lambda f: f.start)


def redact(text: str, vault_hashes: Iterable[str] = ()) -> str:
    """Replace every finding with [REDACTED:<kind>]."""
    findings = scan(text, vault_hashes)
    out, pos = [], 0
    for f in findings:
        out.append(text[pos:f.start])
        out.append(f"[REDACTED:{f.kind}]")
        pos = f.end
    out.append(text[pos:])
    return "".join(out)


def has_secrets(text: str, vault_hashes: Iterable[str] = ()) -> bool:
    return any(f.kind not in _NOT_SECRET for f in scan(text, vault_hashes))
