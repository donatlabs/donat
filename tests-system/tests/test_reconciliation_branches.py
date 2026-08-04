"""When the provider stops answering mid-call, and the store has to find out.

A timeout proves nothing: the money may have moved or it may not. Every money
flow in this store answers the same way — look the operation up, and act on
what the provider says it did. These are the branches that run when the lookup
comes back with "yes, it happened", which is the expensive half to get wrong.
"""

from __future__ import annotations

import uuid

import pytest

from petshop_qa import domain as d
from petshop_qa import providers as P
from petshop_qa import until

pytestmark = [pytest.mark.providers, pytest.mark.serial]


def lookup_answers(providers, outcome: str, *, amount_minor: int, times: int = 10) -> None:
    """The provider's account of an operation it did carry out."""

    providers.script(
        P.LOOKUP,
        times=times,
        patch={
            "found": True,
            "terminal_absence_proven": False,
            "outcome": outcome,
            "provider_mutation_id": f"mutation_{uuid.uuid4().hex[:8]}",
            "provider_event_id": f"evt_{uuid.uuid4().hex[:8]}",
            "provider_reference": f"ref_{uuid.uuid4().hex[:8]}",
            "amount_minor": amount_minor,
        },
    )


def checkout_with_a_silent_provider(shopper, providers, settle_timeout) -> dict:
    """Start a checkout whose authorization call never gets an answer."""

    providers.hang(P.AUTHORIZE, delay_ms=9000, times=3)
    before = {order["id"] for order in d.orders_of(shopper)}
    d.start_checkout(shopper, d.cart_with_one_line(shopper)).unwrap()
    return d.await_new_order(shopper, known=before, timeout=settle_timeout)


# -- checkout ---------------------------------------------------------------


def test_an_authorization_that_did_commit_is_recorded_not_repeated(
    shopper, support, providers, well_stocked, settle_timeout
):
    """The call timed out but the money was taken: the order stands."""

    order = checkout_with_a_silent_provider(shopper, providers, settle_timeout)
    lookup_answers(providers, "authorized", amount_minor=order["total_minor"])

    d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)
    payment = d.await_payment_status(
        support, order["id"], {"authorized"}, timeout=settle_timeout
    )

    assert payment["amount_minor"] == order["total_minor"], (
        "the store records the amount the provider says it took"
    )
    # The point of looking it up: it is not asked for again.
    assert len(providers.calls_about(P.AUTHORIZE, order_id=order["id"])) <= 3, (
        "a reconciled authorization is not re-authorized"
    )


def test_an_authorization_the_provider_declined_ends_the_order(
    shopper, support, providers, well_stocked, settle_timeout
):
    """The call timed out and the answer was no: nothing is owed."""

    order = checkout_with_a_silent_provider(shopper, providers, settle_timeout)
    lookup_answers(providers, "declined", amount_minor=order["total_minor"])

    d.await_order_status(shopper, order["id"], {"cancelled"}, timeout=settle_timeout)
    payment = d.await_payment_status(support, order["id"], {"failed"}, timeout=settle_timeout)

    assert payment["status"] == "failed"
    assert providers.count(P.CAPTURE) == 0, "a declined order is never captured"


# -- cancellation -----------------------------------------------------------


def test_a_void_that_did_commit_is_recorded(
    shopper, support, providers, well_stocked, settle_timeout
):
    """The void call failed, but the provider had already released the hold."""

    order = d.checkout_to_order(
        shopper, d.cart_with_one_line(shopper), timeout=settle_timeout
    )
    d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)

    providers.fail(P.VOID, status=500, times=10)
    lookup_answers(providers, "voided", amount_minor=order["total_minor"])

    shopper.graphql(
        """
        mutation Cancel($order: uuid!, $reason: String!, $request: uuid!) {
          request_authorized_order_cancellation(
            order_id: $order, reason: $reason, request_id: $request
          ) { order_id }
        }
        """,
        {"order": order["id"], "reason": "changed my mind", "request": d.new_request_id()},
    ).unwrap()

    payment = d.await_payment_status(support, order["id"], {"voided"}, timeout=settle_timeout)
    assert payment["status"] == "voided", (
        "a void the provider confirms is recorded even though the call failed"
    )
    d.await_order_status(shopper, order["id"], {"cancelled"}, timeout=settle_timeout)


# -- subscriptions ----------------------------------------------------------


def test_a_renewal_whose_provider_went_quiet_is_reconciled(
    store, support, providers, well_stocked, settle_timeout
):
    """The renewal's authorization timed out; the lookup says it went through."""

    # The lookup answer below is claimed by the first call that matches, so the
    # stand has to be finished with earlier scenarios before it is scripted.
    providers.await_quiet()
    providers.reset()

    worker = store.as_role("subscription_worker")
    subscription = "00000000-0000-0000-0000-0000000000d1"
    occurrence = (
        f"2033-{uuid.uuid4().int % 12 + 1:02d}-{uuid.uuid4().int % 28 + 1:02d}T00:00:00Z"
    )
    providers.hang(P.AUTHORIZE, delay_ms=9000, times=3)
    lookup_answers(providers, "authorized", amount_minor=2499)

    worker.graphql(
        """
        mutation Renew($subscription: uuid!, $occurrence: timestamptz!) {
          start_subscription_renewal(
            subscription_id: $subscription, cron_occurrence: $occurrence
          ) { subscription_id }
        }
        """,
        {"subscription": subscription, "occurrence": occurrence},
    ).unwrap()

    # Asked for by its own occurrence: a stand carries many renewals, and the
    # newest row is not necessarily this test's.
    renewal = until(
        lambda: worker.query(
            """
            query Renewals($occurrence: timestamptz!) {
              subscription_renewal(where: {cron_occurrence: {_eq: $occurrence}}) {
                id status cron_occurrence
              }
            }
            """,
            {"occurrence": occurrence},
        )["subscription_renewal"],
        # `payment_due` is where a renewal sits while the store is still
        # finding out what happened, not an outcome.
        lambda rows: bool(rows) and rows[0]["status"] in {"confirmed", "failed", "paused"},
        timeout=settle_timeout,
        description="the reconciled renewal to settle",
    )[0]

    assert renewal["status"] == "confirmed", (
        f"a renewal the provider says it took is confirmed, not retried: {renewal}"
    )
