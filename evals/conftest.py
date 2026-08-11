"""Make the Petshop driver importable, and load its fixtures.

The eval scenarios drive a store over HTTP exactly as the black-box system
tests do — same roles, same tokens, same steerable providers — so they use the
same driver instead of a second one that would drift from it.
"""

from __future__ import annotations

import pathlib
import sys

_REPO = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(_REPO / "tests-system"))

pytest_plugins = ["petshop_qa.fixtures"]
