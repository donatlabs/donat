"""What the store does when nobody does anything.

Holds expire, reviewers miss deadlines, a failed renewal waits and tries again.
Those periods ship as days, so these run against the fast stand — the same
flows with their declared periods rewritten to seconds (`fast_metadata.py`).
Everything else about the store is unchanged, including the states under test.
"""

from __future__ import annotations

import uuid
from datetime import datetime, timedelta, timezone

import pytest

from petshop_qa import domain as d
from petshop_qa import providers as P
from petshop_qa import until

pytestmark = [pytest.mark.providers, pytest.mark.serial]

SUBSCRIPTION = "00000000-0000-0000-0000-0000000000d1"


def soon(seconds: int) -> str:
    """A deadline the test can outwait, in the store's own clock terms."""

    return (datetime.now(timezone.utc) + timedelta(seconds=seconds)).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )


@pytest.fixture
def fast_shopper(fast_store):
    return fast_store.as_role("customer", d.CUSTOMER_ONE)


@pytest.fixture
def fast_staff(fast_store):
    return fast_store.as_role("staff")


# -- a renewal that keeps being declined ------------------------------------


def test_a_declined_renewal_climbs_the_dunning_ladder(
    fast_store, providers, fast_settle_timeout
):
    """First attempt declined, wait, retry, decline again, wait, retry.

    The ladder is the store's answer to a card that stopped working: it does
    not give up on the first no, and it does not charge in a loop either.
    """

    worker = fast_store.as_role("subscription_worker")
    providers.decline_authorization(times=2)
    occurrence = f"2031-{uuid.uuid4().int % 12 + 1:02d}-{uuid.uuid4().int % 28 + 1:02d}T00:00:00Z"

    worker.graphql(
        """
        mutation Renew($subscription: uuid!, $occurrence: timestamptz!) {
          start_subscription_renewal(
            subscription_id: $subscription, cron_occurrence: $occurrence
          ) { subscription_id status }
        }
        """,
        {"subscription": SUBSCRIPTION, "occurrence": occurrence},
    ).unwrap()

    # Three authorizations: the initial one and both dunning retries. The third
    # succeeds, because only two declines were scripted.
    calls = providers.await_call(P.AUTHORIZE, minimum=3, timeout=fast_settle_timeout)
    assert len(calls) >= 3, "a declined renewal is retried on the declared schedule"

    renewal = until(
        lambda: worker.query(
            """
            query Renewals($subscription: uuid!) {
              subscription_renewal(where: {subscription_id: {_eq: $subscription}},
                                   order_by: {id: desc}, limit: 1) { id status }
            }
            """,
            {"subscription": SUBSCRIPTION},
        )["subscription_renewal"],
        lambda rows: bool(rows) and rows[0]["status"] in {"confirmed", "paused", "failed"},
        timeout=fast_settle_timeout,
        description="the renewal to settle after its retries",
    )[0]
    assert renewal["status"] == "confirmed", (
        f"the retry that the provider accepted renews the subscription: {renewal}"
    )


def test_a_renewal_the_provider_never_accepts_pauses_the_subscription(
    fast_store, providers, fast_settle_timeout
):
    """Every attempt declined: the store stops asking and pauses the plan."""

    worker = fast_store.as_role("subscription_worker")
    providers.decline_authorization(times=10)
    occurrence = f"2032-{uuid.uuid4().int % 12 + 1:02d}-{uuid.uuid4().int % 28 + 1:02d}T00:00:00Z"

    worker.graphql(
        """
        mutation Renew($subscription: uuid!, $occurrence: timestamptz!) {
          start_subscription_renewal(
            subscription_id: $subscription, cron_occurrence: $occurrence
          ) { subscription_id }
        }
        """,
        {"subscription": SUBSCRIPTION, "occurrence": occurrence},
    ).unwrap()

    subscription = until(
        lambda: worker.query(
            """
            query Plan($id: uuid!) { subscription(where: {id: {_eq: $id}}) { id status } }
            """,
            {"id": SUBSCRIPTION},
        )["subscription"][0],
        lambda row: row["status"] in {"paused", "payment_due"},
        timeout=fast_settle_timeout,
        description="the subscription to stop being charged",
    )

    assert subscription["status"] in {"paused", "payment_due"}, (
        "a card that never works pauses the plan instead of retrying forever"
    )
    # And the store stopped asking: three attempts, not a loop.
    assert providers.count(P.AUTHORIZE) <= 3, (
        f"the ladder has a top: {providers.count(P.AUTHORIZE)} authorizations"
    )


# -- holds and reviews nobody comes back to ---------------------------------


def test_an_unconfirmed_grooming_hold_expires(fast_shopper, providers, fast_settle_timeout):
    slot = f"2030-05-06T09:{uuid.uuid4().hex[:2]}"
    fast_shopper.graphql(
        """
        mutation Hold(
          $resource: uuid!, $slot: String!, $starts: timestamptz!,
          $expires: timestamptz!, $request: uuid!
        ) {
          start_grooming_booking(
            service_resource_id: $resource, slot_key: $slot, starts_at: $starts,
            hold_expires_at: $expires, request_id: $request
          ) { slot_key }
        }
        """,
        {
            "resource": str(uuid.uuid4()),
            "slot": slot,
            "starts": soon(600),
            # The hold lapses in seconds, because the shopper never confirms.
            "expires": soon(6),
            "request": d.new_request_id(),
        },
    ).unwrap()

    expired = until(
        lambda: fast_shopper.query(
            """
            query Bookings($slot: String!) {
              grooming_booking(where: {slot_key: {_eq: $slot}}) { id status }
            }
            """,
            {"slot": slot},
        )["grooming_booking"],
        lambda rows: bool(rows) and rows[0]["status"] not in {"held"},
        timeout=fast_settle_timeout,
        description="the unconfirmed hold to expire",
    )[0]

    assert expired["status"] == "expired", (
        f"a hold nobody confirms releases the groomer's morning: {expired}"
    )


def test_a_prescription_nobody_reviews_expires(
    fast_shopper, fast_store, fast_staff, providers, fast_settle_timeout
):
    d.ensure_stock(fast_staff, d.IN_STOCK_VARIANT)
    order = d.checkout_to_order(
        fast_shopper, d.cart_with_one_line(fast_shopper), timeout=fast_settle_timeout
    )
    d.await_order_status(fast_shopper, order["id"], {"authorized"}, timeout=fast_settle_timeout)
    line_id = fast_shopper.query(
        "query Lines($order: uuid!) { orders(where: {id: {_eq: $order}}) { lines { id } } }",
        {"order": order["id"]},
    )["orders"][0]["lines"][0]["id"]

    fast_shopper.graphql(
        """
        mutation Review($line: uuid!, $deadline: timestamptz!, $request: uuid!) {
          start_prescription_review(
            order_line_id: $line, review_deadline: $deadline, request_id: $request
          ) { order_line_id }
        }
        """,
        {
            "line": line_id,
            # The reviewer has seconds, and does not use them.
            "deadline": soon(6),
            "request": d.new_request_id(),
        },
    ).unwrap()

    decided = until(
        lambda: fast_shopper.query(
            """
            query Requests($line: uuid!) {
              prescription_request(where: {order_line_id: {_eq: $line}}) { id status }
            }
            """,
            {"line": line_id},
        )["prescription_request"],
        lambda rows: bool(rows) and rows[0]["status"] != "submitted",
        timeout=fast_settle_timeout,
        description="the unreviewed prescription to expire",
    )[0]

    assert decided["status"] == "expired", (
        f"a review nobody makes does not hold the line forever: {decided}"
    )


@pytest.mark.xfail(
    strict=True,
    reason=(
        "a return nobody approves ends its Process in `approval_timed_out` and "
        "writes nothing to the domain: the shopper's return_request stays "
        "`requested` for ever, and because the Process is gone a later approval "
        "can no longer refund it — the request is orphaned in a state that "
        "reads like it is still being considered"
    ),
)
def test_a_return_support_never_answers_times_out(
    fast_shopper, fast_store, fast_staff, providers, fast_settle_timeout
):
    support = fast_store.as_role("support")
    fulfilment = fast_store.as_role("fulfilment")
    d.ensure_stock(fast_staff, d.IN_STOCK_VARIANT)

    order = d.checkout_to_order(
        fast_shopper, d.cart_with_one_line(fast_shopper), timeout=fast_settle_timeout
    )
    d.await_order_status(fast_shopper, order["id"], {"authorized"}, timeout=fast_settle_timeout)
    fulfilment.graphql(
        """
        mutation Fulfil($order: uuid!, $request: uuid!) {
          start_order_fulfilment(
            order_id: $order, destination_region: "northeast", allocation_request_id: $request
          ) { order_id }
        }
        """,
        {"order": order["id"], "request": d.new_request_id()},
    ).unwrap()
    d.await_payment_status(support, order["id"], {"captured"}, timeout=fast_settle_timeout)

    line_id = fast_shopper.query(
        "query Lines($order: uuid!) { orders(where: {id: {_eq: $order}}) { lines { id } } }",
        {"order": order["id"]},
    )["orders"][0]["lines"][0]["id"]
    fast_shopper.graphql(
        """
        mutation Start($order: uuid!, $line: uuid!, $request: uuid!) {
          start_return(
            order_id: $order, lines: [{order_line_id: $line, requested_quantity: 1}],
            reason: "changed my mind", replacement_requested: false, request_id: $request
          ) { order_id }
        }
        """,
        {"order": order["id"], "line": line_id, "request": d.new_request_id()},
    ).unwrap()

    # Nobody approves it. The store must not leave the shopper waiting forever.
    settled = until(
        lambda: support.query(
            """
            query Returns($order: uuid!) {
              return_request(where: {order_id: {_eq: $order}}) { id status }
            }
            """,
            {"order": order["id"]},
        )["return_request"],
        lambda rows: bool(rows) and rows[0]["status"] not in {"requested"},
        timeout=fast_settle_timeout,
        description="the unanswered return to time out",
    )[0]

    assert settled["status"] in {"rejected", "timed_out", "expired", "cancelled"}, (
        f"an unanswered return reaches a decision of its own: {settled}"
    )
    assert providers.count(P.REFUND) == 0, "a timed-out return refunds nothing"
