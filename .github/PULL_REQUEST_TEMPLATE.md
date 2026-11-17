## What and why

<!-- What does this change, and why? Link the issue: Closes #123 -->

## How it was tested

<!-- Tests added or changed, plus any manual steps (for example, `python -m allforone.demo`). -->

## Checklist

- [ ] `pytest` passes locally (on fakes, with no credentials)
- [ ] `ruff check .` and `ruff format --check .` pass
- [ ] **PII scan passes:** `python scripts/pii_scan.py` exits 0
- [ ] **No real client data** in code, fixtures, docs, screenshots or logs (demo data only: Northwind Supplies, Alex / Maya / Sam, `example.com`)
- [ ] No secrets, keys or tokens committed; new secrets are read from the credentials store
- [ ] New or changed connectors have an interface, a fake and contract tests, and return `blocked` (never a fake result) when credentials are missing
- [ ] Safety rules intact: the ack is the only automatic message, every other send needs a bound approval, no deletes through the helpdesk guard, and redaction runs before judging
- [ ] Docs updated (README, config example) if behaviour or config changed
- [ ] Commits are signed off (`git commit -s`, DCO)

## Notes for the reviewer

<!-- Risky areas, open questions, follow-ups. -->
