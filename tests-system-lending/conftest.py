"""Fixtures for the black-box lending suite.

Every test runs against each configured stand. That is the point of this
suite: the same YAML is served by the standalone Rust engine and by a Go
application that embeds the compiled core, and the two must not disagree. A
test that passes on one and fails on the other is reported per stand, so the
failure names which host is wrong.

Without any stand configured the whole suite skips: a run with no service must
not look like a passing run, and must not look like a broken library either.
"""

from __future__ import annotations

import uuid

import pytest

from lending_qa import Library, Stand

STANDS = Stand.from_env()


def pytest_report_header(config):  # pragma: no cover - reporting only
    if not STANDS:
        return (
            "lending: no stand configured "
            "(set LENDING_ENGINE_URL and/or LENDING_GO_URL)"
        )
    return "lending: " + ", ".join(f"{s.name}={s.base_url}" for s in STANDS)


@pytest.fixture(params=[s.name for s in STANDS] or [None])
def library(request) -> Library:
    """One library, on a fresh member, on each configured stand."""
    if request.param is None:
        pytest.skip(
            "no lending stand configured: raise one with "
            "tests-system-lending/stack.sh up"
        )
    stand = next(s for s in STANDS if s.name == request.param)
    lib = Library(stand)
    if not lib.is_up():
        pytest.fail(
            f"no lending service answering at {stand.base_url}; "
            "raise one with tests-system-lending/stack.sh up"
        )
    # Every test gets its own member rather than a shared reset, so the stands
    # can share a database if an operator points both at one, and so a failed
    # test cannot strand another test's fixtures.
    lib.add_member(f"member-{uuid.uuid4().hex[:8]}", 3)
    return lib


@pytest.fixture
def shelf(library: Library):
    """A helper that puts `n` fresh copies of one title on the shelf."""

    def _shelf(n: int = 1) -> list[str]:
        book = library.add_book(f"Book {uuid.uuid4().hex[:6]}", "A. Author")
        return [library.add_copy(book) for _ in range(n)]

    return _shelf
