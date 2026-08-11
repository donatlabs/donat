"""Fixtures for the black-box Petshop suite.

They live in `petshop_qa.fixtures` so that the eval scenarios can load the same
ones; this file only registers them for this suite.
"""

from __future__ import annotations

pytest_plugins = ["petshop_qa.fixtures"]
