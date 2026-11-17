# Security Policy

All for One handles client messages, voice notes and helpdesk data, so we take security reports seriously.

## Reporting a vulnerability

**Please do not open a public issue, discussion or PR for a security problem.**

Report it privately through GitHub Security Advisories:

1. Go to the repository's **Security** tab.
2. Click **Report a vulnerability**. The direct link is https://github.com/Shadysmetools/all-for-one-hub/security/advisories/new.
3. Include:
   - the affected version or commit;
   - steps to reproduce;
   - the impact as you understand it;
   - any proof of concept.

   Use synthetic data only. Never include real client messages, phone numbers or credentials.

**What to expect:**
- We acknowledge your report within **3 business days**.
- We send an initial assessment within **10 business days**.
- We keep you updated until the issue is fixed.
- We coordinate a disclosure date with you, normally within **90 days** of the report.
- With your permission, we credit you in the advisory.

## Supported versions

The project is pre-1.0. Only the latest release and `main` get security fixes.

| Version | Supported |
|---|---|
| `main` | yes |
| latest `0.x` release | yes |
| older `0.x` releases | no |

## Scope

In scope: code in this repository, especially these areas:
- **Webhook verification:** the HMAC check, the verify token, body limits, accepting only the configured number.
- **The approval model:** binding to the approver, ticket, kind, draft hash, nonce and expiry; single use; any path that sends a client message without a valid approval; any second automatic message beyond the one ack.
- **The helpdesk guard:** any way to delete records, reach models outside the allowlist, or trigger mail on create.
- **PII handling:** redaction before judge calls; phone numbers or message text leaking into logs or learning records.
- **Secrets handling:** credentials exposed through config, logs, errors or process environment.
- **Prompt injection:** client message content that changes routing, approvals or sends.
- **The MCP and API surfaces**, once they ship. These are read and propose only, and must never send, approve or delete.

Out of scope:
- vulnerabilities in third-party services (Meta, Telegram, Odoo, model providers) — report those to the vendor;
- issues that need a compromised host or stolen maintainer credentials;
- weaknesses in a deployment that ignores the documented reference design;
- the commercial edition, which is not in this repository;
- denial of service through volume alone, social engineering, and physical attacks.

## Safe harbour

We won't pursue legal action against good-faith research that:
- follows this policy;
- avoids privacy violations and service disruption;
- uses only test data and accounts you own.
