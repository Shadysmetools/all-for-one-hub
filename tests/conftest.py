# SPDX-License-Identifier: Apache-2.0
"""Keep provider credentials out of the test run: every adapter must behave as if no key were set."""
import pytest


@pytest.fixture(autouse=True)
def _no_provider_keys(monkeypatch):
    for name in ("GROQ_API_KEY", "VERTEX_PROJECT", "GOOGLE_APPLICATION_CREDENTIALS", "OPENAI_API_KEY",
                 "ODOO_URL", "ODOO_DB", "ODOO_LOGIN", "ODOO_API_KEY"):
        monkeypatch.delenv(name, raising=False)
