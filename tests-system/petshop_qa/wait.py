"""Waiting for work the store does after it answers.

A durable Process keeps running once the entry-point Command has replied, so an
outside tester observes its outcome by re-reading the store until the expected
state appears. Every wait is bounded and reports the last thing it saw.
"""

from __future__ import annotations

import time
from typing import Any, Callable, TypeVar

T = TypeVar("T")


class Unsettled(AssertionError):
    """The store did not reach the expected state within the budget."""


def until(
    probe: Callable[[], T],
    accept: Callable[[T], bool],
    *,
    timeout: float,
    description: str,
    interval: float = 0.25,
) -> T:
    """Poll `probe` until `accept` holds, then return the accepted observation."""

    deadline = time.monotonic() + timeout
    last: Any = None
    attempts = 0
    while True:
        last = probe()
        attempts += 1
        if accept(last):
            return last
        if time.monotonic() >= deadline:
            raise Unsettled(
                f"{description} did not happen within {timeout:g}s "
                f"({attempts} observations); last seen: {last!r}"
            )
        time.sleep(interval)


def value_of(
    probe: Callable[[], T],
    *,
    timeout: float,
    description: str,
    interval: float = 0.25,
) -> T:
    """Poll until `probe` returns something other than None/empty."""

    return until(
        probe,
        lambda observed: observed not in (None, [], {}, ""),
        timeout=timeout,
        description=description,
        interval=interval,
    )


def stays(
    probe: Callable[[], T],
    accept: Callable[[T], bool],
    *,
    duration: float,
    description: str,
    interval: float = 0.25,
) -> None:
    """Assert a condition holds for a whole window, not just once.

    Used where the interesting claim is that nothing further happens — no late
    authorization, no second charge, no resurrected reservation.
    """

    deadline = time.monotonic() + duration
    while time.monotonic() < deadline:
        observed = probe()
        if not accept(observed):
            raise AssertionError(f"{description} stopped holding; saw: {observed!r}")
        time.sleep(interval)
