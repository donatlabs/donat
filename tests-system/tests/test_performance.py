"""What the store costs to use, measured as ratios rather than milliseconds.

An absolute budget ("under 50ms") means one thing on a workstation and another
in CI, so it either fails for no reason or passes while everything regresses.
Every case here compares the store against itself instead: the same work done
two ways, or the same query with and without the store being busy. The machine
cancels out, and what is left is the property — that requests overlap, that a
busy queue does not starve the shop front, that a page of rows is one question
rather than a hundred.

Each threshold below carries the margin it was chosen with, so the next person
can tell a real regression from a slow afternoon.
"""

from __future__ import annotations

import statistics
import threading
import time
import uuid

import pytest

from petshop_qa import domain as d

pytestmark = pytest.mark.serial

#: Enough samples for a median to mean something, few enough to stay quick.
SAMPLES = 25

CATALOGUE = """
    query {
      product(order_by: {id: asc}) {
        id slug title
        category { id name }
        variants { id sku }
      }
    }
"""

BOOKING = """
    mutation Hold(
      $resource: uuid!, $slot: String!, $starts: timestamptz!,
      $expires: timestamptz!, $request: uuid!
    ) {
      start_grooming_booking(
        service_resource_id: $resource, slot_key: $slot, starts_at: $starts,
        hold_expires_at: $expires, request_id: $request
      ) { slot_key }
    }
"""


def milliseconds(call) -> float:
    started = time.perf_counter()
    call()
    return (time.perf_counter() - started) * 1000


def latencies(call, samples: int = SAMPLES) -> list[float]:
    """Timings with the first few thrown away: a cold connection is not news."""

    for _ in range(3):
        call()
    return [milliseconds(call) for _ in range(samples)]


def p95(values: list[float]) -> float:
    ordered = sorted(values)
    return ordered[max(0, int(0.95 * len(ordered)) - 1)]


def hold_a_slot(shopper) -> None:
    """One durable flow: a command, and the Process it starts."""

    shopper.graphql(
        BOOKING,
        {
            "resource": str(uuid.uuid4()),
            "slot": f"2034-01-01T10:{uuid.uuid4().hex[:6]}",
            "starts": "2034-01-01T10:00:00Z",
            "expires": "2034-01-01T09:00:00Z",
            "request": d.new_request_id(),
        },
    )


def test_the_store_serves_requests_in_parallel_not_in_turn(store, shopper, providers):
    """Twelve callers at once must not take twelve callers' worth of time.

    The ratio is the assertion: whatever one booking costs on this machine, a
    dozen sent together has to cost far less than a dozen sent one after
    another. A store answering in turn would sit at 1.0; measured at 0.24-0.32.

    The two measurements are taken alternately, five rounds, and the *best*
    ratio is what is asserted. Taking them once each was not stable: a store
    still draining an earlier scenario's durable work is slower for whichever
    measurement happens to run then, and the ratio moved with the drift rather
    than with the store. Alternating puts both halves of each ratio in the same
    conditions.

    The best rather than the median, because on a shared runner the noise is
    one-sided: whatever else is competing for those cores can only stop twelve
    callers from overlapping, never make twelve serialised ones overlap. So a
    single clean round proves the store can answer in parallel, while a store
    that truly answers in turn sits at 1.0 in every round and cannot produce a
    low one. A median of three did not survive that — 0.73, 0.31, 0.62 on a
    runner measured at 0.24-0.32 when it is left alone.

    Twelve real callers, each with its own connection: one shared client would
    measure the test's own socket pool as much as the store.
    """

    rounds = 12
    callers = [store.as_role("customer", d.CUSTOMER_ONE) for _ in range(rounds)]
    hold_a_slot(shopper)  # warm

    def serially() -> float:
        return milliseconds(lambda: [hold_a_slot(shopper) for _ in range(rounds)])

    def together() -> float:
        threads = [threading.Thread(target=hold_a_slot, args=(caller,)) for caller in callers]
        return milliseconds(
            lambda: ([thread.start() for thread in threads], [thread.join() for thread in threads])
        )

    ratios = []
    for _ in range(5):
        one_at_a_time = serially()
        all_at_once = together()
        ratios.append(all_at_once / one_at_a_time)

    best = min(ratios)
    assert best < 0.6, (
        f"twelve callers together never cost less than {best:.2f} of what they "
        f"cost one at a time (rounds: {[round(ratio, 2) for ratio in ratios]}): "
        "the store is answering them in turn"
    )


def test_durable_work_in_flight_does_not_slow_the_shop_front(shopper, anonymous):
    """A busy queue is the store's problem, not the shopper's.

    The durable workers and the API draw on the same connection pool, so work
    the shopper never asked to see could in principle make the catalogue wait.
    Forty flows are put in flight and the catalogue is read while they drain.

    Today it does not come close: p95 under load is about the same as idle
    (5.6ms against 4.4ms), and stays that way even with the workers configured
    to take all but one connection of the pool — a transition holds its
    connection only while it applies. So this does not currently catch anything;
    it is here for the change that would, the day a transition starts holding a
    connection for longer. The ceiling is four times idle plus 20ms.
    """

    idle = latencies(lambda: anonymous.query(CATALOGUE))

    busy = [threading.Thread(target=hold_a_slot, args=(shopper,)) for _ in range(40)]
    for caller in busy:
        caller.start()
    try:
        under_load = latencies(lambda: anonymous.query(CATALOGUE))
    finally:
        for caller in busy:
            caller.join()

    ceiling = p95(idle) * 4 + 20
    assert p95(under_load) < ceiling, (
        f"the catalogue slowed to p95 {p95(under_load):.0f}ms while the store was "
        f"working (idle p95 {p95(idle):.0f}ms): durable work is starving the API"
    )


def test_a_page_of_rows_is_one_question_not_a_hundred(support):
    """The whole answer is assembled in the database, or this collapses.

    Forty payments with their order and their refunds, fetched in one query
    against the same forty fetched one at a time. If the engine ever walked
    relationships row by row, the batched form would drift toward the
    one-by-one form — which is exactly what an N+1 regression looks like from
    outside. Measured at 20× when this was written; the floor is 5×.
    """

    rows = support.query("query { payment(limit: 40) { id } }")["payment"]
    identifiers = [row["id"] for row in rows]
    if len(identifiers) < 10:
        pytest.skip(f"this test needs a few payments to page through, saw {len(identifiers)}")

    one_at_a_time = """
        query One($id: uuid!) {
          payment(where: {id: {_eq: $id}}) {
            id status amount_minor
            order { id order_status }
            refunds { id amount_minor }
          }
        }
    """
    all_at_once = """
        query Many($ids: [uuid!]) {
          payment(where: {id: {_in: $ids}}) {
            id status amount_minor
            order { id order_status }
            refunds { id amount_minor }
          }
        }
    """

    separately = statistics.median(
        latencies(
            lambda: [support.query(one_at_a_time, {"id": identifier}) for identifier in identifiers],
            samples=3,
        )
    )
    together = statistics.median(latencies(lambda: support.query(all_at_once, {"ids": identifiers})))

    assert together * 5 < separately, (
        f"one query for {len(identifiers)} rows cost {together:.1f}ms against "
        f"{separately:.1f}ms row by row: the answer is not being assembled in one place"
    )


def test_a_rarely_used_role_does_not_pay_to_be_first(store):
    """Every role's schema is ready before the first request arrives.

    A store compiles one schema per role. If that happened on demand, the first
    caller in a rare role — a veterinarian, a payout worker — would pay for it,
    and would pay again after every deploy. Measured at 1.1× the warm median,
    i.e. no penalty beyond noise; the ceiling is five times warm plus 50ms.
    """

    reviewer = store.as_role("veterinary_reviewer", "veterinarian-1")
    query = "query { prescription_request(limit: 1) { id status } }"

    first = milliseconds(lambda: reviewer.query(query))
    warm = statistics.median(latencies(lambda: reviewer.query(query)))

    assert first < warm * 5 + 50, (
        f"the first request in a rarely used role cost {first:.0f}ms against a "
        f"warm {warm:.0f}ms: the role's schema is being built when it is asked for"
    )
