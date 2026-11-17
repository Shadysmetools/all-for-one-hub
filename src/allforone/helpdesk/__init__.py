# SPDX-License-Identifier: Apache-2.0
"""Helpdesk layer: the write guard, the guarded gateway, a read-only Odoo adapter and an in-memory fake.

Real Odoo write-back (create / write tickets, notes, assignment against a live server) is part of the commercial
edition. This package ships the guard, the write interface and a fake, plus a READ-ONLY Odoo adapter.
"""
