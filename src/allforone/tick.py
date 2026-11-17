# SPDX-License-Identifier: Apache-2.0
"""Diagnostics for ``Hub.tick``: ``python -m allforone.tick --dry-run`` runs one tick against an ALL-FAKES hub on an
IN-MEMORY store (never a real ``hub.db``, no network, no way to point it at real state).

The production tick is NOT a separate process: ``HubServer`` (webhook.py) runs ``hub.tick()`` on a timer thread in
the SAME process as the webhook workers, because the one-ticket / one-ack guarantees are per process (and
``SqliteStore.lock_exclusive`` refuses a second hub on the same state file). Do not wire this CLI to a system timer.
A run without ``--dry-run`` is BLOCKED (exit 2) with a plain reason, never a silent fake.
"""
from __future__ import annotations

import argparse
import json
import sys


def _fake_hub():
    from .config import demo_config
    from .store import SqliteStore
    from .wiring import build_fake_hub
    store = SqliteStore(":memory:")
    return build_fake_hub({**demo_config(), "phone_number_id": "dry-run"}, store=store), store


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="allforone.tick")
    ap.add_argument("--dry-run", action="store_true", help="all-fakes hub (no network)")
    args = ap.parse_args(argv)
    if not args.dry_run:
        print("blocked: real hub adapters are not configured (production Hub factory not wired); "
              "use --dry-run to exercise the tick with fakes", file=sys.stderr)
        return 2
    hub, store = _fake_hub()
    try:
        print(json.dumps(hub.tick()))
    finally:
        store.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
