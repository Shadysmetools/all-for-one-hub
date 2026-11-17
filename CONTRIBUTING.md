# Contributing to All for One

Thanks for helping. This guide covers setup, the checks every PR must pass, and how to sign your work.

## Ground rules

- **No real client data, ever.** That covers fixtures, screenshots, logs, voice notes and issue text. Use the demo company "Northwind Supplies", the demo team (Alex, Maya, Sam), `example.com` emails, and phone numbers from reserved test ranges.
- **Every connector has an interface and a fake.** New adapters ship with a fake and contract tests, so CI keeps running with zero credentials.
- **Never let production fall back to a fake silently.** A real adapter with no credentials returns `blocked` with a reason.
- **Keep the safety rules intact.** The ack is the only automatic client message, every other send needs a bound human approval, and the helpdesk guard allows no deletes. A PR that weakens any of these needs a maintainer's sign-off in the PR thread.

## Dev setup

```bash
git clone https://github.com/Shadysmetools/all-for-one-hub.git
cd all-for-one-hub
python -m venv .venv && source .venv/bin/activate   # Windows: .venv\Scripts\activate
pip install -e ".[dev]"
python -m allforone.demo                            # sanity check: the full loop on fakes
```

Python 3.11+ is required. No API keys are needed for development or tests.

## Checks (all must pass before a PR)

```bash
pytest                       # the full suite runs on fakes
ruff check . && ruff format --check .
python scripts/pii_scan.py   # must exit 0
```

**PII scan.** `scripts/pii_scan.py` looks for phone numbers, email addresses outside `example.com`, IP addresses, likely secrets and tokens, and a denylist of real names kept by the maintainers. It scans the code, fixtures, docs and test data. If it flags a false positive, add a narrowly scoped allow entry with a comment explaining why. Don't disable the check.

**Tests first.** For bug fixes, add a failing test that reproduces the bug, then the fix. For new behaviour, include tests that cover the safety rules it touches (approvals, the one-ack rule, redaction, the guard).

## Commit style

- [Conventional Commits](https://www.conventionalcommits.org/): `feat:`, `fix:`, `docs:`, `test:`, `refactor:`, `chore:`, with an optional scope, such as `fix(approvals): reject expired nonce`.
- Subject line in the imperative mood, 72 characters or fewer.
- One logical change per commit. Explain *why* in the body when it isn't obvious.

## Sign-off (DCO required)

Contributions are accepted under the [Developer Certificate of Origin](https://developercertificate.org/). Sign every commit:

```bash
git commit -s -m "fix(webhook): reject oversized bodies before parsing"
```

This adds a `Signed-off-by: Your Name <you@example.com>` line, which certifies that you have the right to submit the change under the project license. PRs with unsigned commits can't be merged. To sign earlier commits, run `git rebase --signoff main`.

## License of contributions

This repository is licensed under [Apache-2.0](LICENSE). Contributions are accepted under the same license (**inbound = outbound**), as described in section 5 of the Apache License.

## Pull requests

- Open an issue first for anything larger than a small fix, so we can agree on the approach.
- Fill in the PR template checklist.
- Every PR gets an independent review before merge.
- Keep PRs focused, and avoid unrelated formatting churn.

## Reporting security issues

Don't open a public issue. Follow [SECURITY.md](SECURITY.md).
