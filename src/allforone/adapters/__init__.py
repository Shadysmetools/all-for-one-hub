# SPDX-License-Identifier: Apache-2.0
"""Optional real adapters. Each imports its third-party SDK lazily and blocks itself (plain reason) when its key or
dependency is missing, never falling back to a fake. Extras: ``[groq]``, ``[telegram]``, ``[vertex]``."""
