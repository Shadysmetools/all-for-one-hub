#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Regex scan for personal data and secrets in tracked files.

Finds: phone numbers, emails (anything not @example.com), IPv4 addresses, token shapes (API keys, bearer tokens,
private keys, long hex secrets), and a DENYLIST of names (company, people, customers).

The denylist is never committed. It is read from the ``ALLFORONE_DENYLIST`` environment variable (comma or newline
separated) and/or a local, untracked ``.denylist`` file (one entry per line, ``#`` comments allowed). Matching is
case-insensitive. Findings print file, line and kind only (never the matched denylist word).

Usage:  python scripts/pii_scan.py [paths...]      exit code 1 when anything is found
Allow a reviewed line by adding ``pii-scan: allow`` in a comment on it.
"""
from __future__ import annotations

import ipaddress
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SKIP_DIRS = {".git", "__pycache__", ".pytest_cache", ".ruff_cache", ".venv", "node_modules", "dist", "build", "out"}
SKIP_FILES = {".denylist", ".gitleaksignore"}  # .gitleaksignore holds scanner fingerprints (commit SHAs), not data
SKIP_SUFFIXES = {".wav", ".png", ".jpg", ".gif", ".mp4", ".pyc", ".db", ".zip"}
ALLOW_MARK = "pii-scan: allow"

EMAIL = re.compile(r"\b[A-Za-z0-9._%+-]+@([A-Za-z0-9.-]+\.[A-Za-z]{2,})\b")
IPV4 = re.compile(r"(?<![\d.])(?:\d{1,3}\.){3}\d{1,3}(?![\d.])")
PHONE_PLUS = re.compile(r"(?<![\w])\+\d[\d ().-]{7,}\d")
PHONE_SEP = re.compile(r"(?<![\w.])\(?\d{3}\)?[-. ]\d{3}[-. ]\d{4}(?![\w.])")
PHONE_BARE = re.compile(r"(?<![\w.])\d{11,15}(?![\w.])")
FAKE_PHONE = re.compile(r"^\+?1[-. ]?\(?555\)?[-. ]?01\d\d")          # +1-555-01xx is reserved for fiction
FAKE_BARE = re.compile(r"^155501\d{5,9}$")
TOKENS = [
    ("api_key", re.compile(r"\b(?:sk|pk|rk|xox[abp]|ghp|gho|ghs|github_pat|AKIA|AIza)[A-Za-z0-9_\-]{16,}")),
    ("bot_token", re.compile(r"\b\d{8,10}:[A-Za-z0-9_-]{35}\b")),
    ("bearer", re.compile(r"(?i)\bbearer\s+[A-Za-z0-9\-._~+/]{20,}=*")),
    ("jwt", re.compile(r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}")),
    ("private_key", re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----")),
    ("hex_secret", re.compile(r"\b(?=[0-9a-f]*[a-f])(?=[0-9a-f]*\d)(?:[0-9a-f]{40}|[0-9a-f]{32})\b")),
    ("secret_assignment", re.compile(r"(?i)\b(?:api[_-]?key|secret|token|password)\s*[:=]\s*[\"'][A-Za-z0-9/+_\-]{16,}[\"']")),
]
PRIVATE_OK = ("127.", "0.0.0.0", "192.0.2.", "198.51.100.", "203.0.113.")


def load_denylist(root: Path = ROOT) -> list[str]:
    words: list[str] = []
    raw = os.environ.get("ALLFORONE_DENYLIST", "")
    words += [w.strip() for w in re.split(r"[,\n]", raw)]
    f = root / ".denylist"
    if f.is_file():
        words += [ln.strip() for ln in f.read_text(encoding="utf-8").splitlines() if not ln.lstrip().startswith("#")]
    return sorted({w.lower() for w in words if len(w) >= 2})


def tracked_files(root: Path = ROOT) -> list[Path]:
    try:
        out = subprocess.run(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=root,
                             capture_output=True, check=True).stdout.decode("utf-8")
        return [root / p for p in out.split("\0") if p]
    except (OSError, subprocess.CalledProcessError):
        return [p for p in root.rglob("*") if p.is_file() and not set(p.relative_to(root).parts) & SKIP_DIRS]


def _ip_findings(line: str) -> list[str]:
    out = []
    for m in IPV4.finditer(line):
        try:
            ipaddress.IPv4Address(m.group(0))
        except ValueError:
            continue
        if not m.group(0).startswith(PRIVATE_OK):
            out.append("ipv4")
    return out


def _phone_findings(line: str) -> list[str]:
    out = []
    for rx, bare in ((PHONE_PLUS, False), (PHONE_SEP, False), (PHONE_BARE, True)):
        for m in rx.finditer(line):
            s = m.group(0)
            digits = re.sub(r"\D", "", s)
            if bare and (FAKE_BARE.match(digits) or digits.startswith("0")):
                continue
            if not bare and (FAKE_PHONE.match(s) or FAKE_BARE.match(digits)):
                continue
            out.append("phone")
    return out


def scan_line(line: str, denylist: list[str]) -> list[str]:
    if ALLOW_MARK in line:
        return []
    kinds: list[str] = []
    kinds += ["email" for m in EMAIL.finditer(line) if m.group(1).lower() != "example.com"]
    kinds += _ip_findings(line)
    kinds += _phone_findings(line)
    kinds += [k for k, rx in TOKENS if rx.search(line)]
    low = line.lower()
    kinds += ["denylist" for w in denylist if w in low]
    return kinds


def scan_file(path: Path, denylist: list[str]) -> list[tuple[int, str]]:
    if path.name in SKIP_FILES or path.suffix.lower() in SKIP_SUFFIXES:
        return []
    try:
        text = path.read_text(encoding="utf-8")
    except (UnicodeDecodeError, OSError):
        return []
    return [(n, k) for n, line in enumerate(text.splitlines(), 1) for k in scan_line(line, denylist)]


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    files = [Path(a) for a in args] if args else tracked_files()
    deny = load_denylist()
    if not deny:
        print("note: no denylist loaded (set ALLFORONE_DENYLIST or create a local .denylist)", file=sys.stderr)
    total = 0
    for f in sorted(files):
        for line_no, kind in scan_file(f, deny):
            rel = f.relative_to(ROOT) if f.is_absolute() and ROOT in f.parents else f
            print(f"{rel}:{line_no}: {kind}")
            total += 1
    print(f"pii_scan: {total} finding(s) in {len(files)} file(s)")
    return 1 if total else 0


if __name__ == "__main__":
    sys.exit(main())
