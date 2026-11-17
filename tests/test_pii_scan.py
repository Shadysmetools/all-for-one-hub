# SPDX-License-Identifier: Apache-2.0
"""The PII scanner: flags real-looking data, allows the reserved fictional shapes, reads the denylist locally."""
from __future__ import annotations

import importlib.util
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("pii_scan", Path(__file__).parent.parent / "scripts" / "pii_scan.py")
pii = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(pii)


def kinds(line, deny=()):
    return pii.scan_line(line, list(deny))


def test_flags_phones_emails_ips_and_tokens():
    assert "phone" in kinds("call +44 7700 900123 now")  # pii-scan: allow
    assert "email" in kinds("write to person@corp.io")  # pii-scan: allow
    assert "ipv4" in kinds("host 10.20.30.40")  # pii-scan: allow
    assert "api_key" in kinds("key sk-" + "a1b2c3d4e5f6g7h8i9j0k1l2")
    assert "bot_token" in kinds("1234567890:" + "A" * 35)


def test_allows_fictional_shapes_and_documentation_addresses():
    for ok in ("+1-555-0100", "15550100001", "me@example.com", "127.0.0.1", "203.0.113.9", "ts 1762857600",
               "a line with pii-scan: allow person@corp.io"):
        assert kinds(ok) == [], ok


def test_denylist_matches_case_insensitively_and_never_echoes_the_word(tmp_path, monkeypatch, capsys):
    monkeypatch.setenv("ALLFORONE_DENYLIST", "Globex, Initech")
    (tmp_path / ".denylist").write_text("# comment\nHooli\n", encoding="utf-8")
    deny = pii.load_denylist(tmp_path)
    assert deny == ["globex", "hooli", "initech"]
    assert kinds("shipped to GLOBEX", deny) == ["denylist"]
    f = tmp_path / "a.txt"
    f.write_text("hello initech\n", encoding="utf-8")
    assert pii.main([str(f)]) == 1
    assert "initech" not in capsys.readouterr().out.lower().replace("pii_scan", "")


def test_the_repository_itself_is_clean():
    assert pii.main([]) == 0
