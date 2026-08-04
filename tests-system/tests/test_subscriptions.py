"""A subscription renews itself, and the shopper is charged once for it."""

from __future__ import annotations

import random
from datetime import datetime, timedelta, timezone

import pytest

from petshop_qa import domain as d
from petshop_qa import providers as P
from petshop_qa import until

pytestmark = pytest.mark.providers

#: Provisioned by the stand: an active subscription for the first customer.
SUBSCRIPTION = "00000000-0000-0000-0000-0000000000d1"


def renew(worker, occurrence: str, subscription: str = SUBSCRIPTION):
    return worker.graphql(
        """
        mutation Renew($subscription: uuid!, $occurrence: timestamptz!) {
          start_subscription_renewal(
            subscription_id: $subscription, cron_occurrence: $occurrence
          ) { subscription_id status }
        }
        """,
        {"subscription": subscription, "occurrence": occurrence},
    )


def renewals(worker, subscription: str = SUBSCRIPTION) -> list[dict]:
    return worker.query(
        """
        query Renewals($subscription: uuid!) {
          subscription_renewal(where: {subscription_id: {_eq: $subscription}}) {
            id cron_occurrence status
          }
        }
        """,
        {"subscription": subscription},
    )["subscription_renewal"]


def occurrence() -> str:
    """A schedule slot no run has used before.

    A renewal is idempotent per occurrence — that is the point of the second
    case below — so a fixed slot would silently stop charging on the second
    run against a long-lived stand.
    """

    minute = random.randrange(0, 60 * 24 * 365)
    return (datetime(2030, 1, 1, tzinfo=timezone.utc)
            + timedelta(minutes=minute)).strftime("%Y-%m-%dT%H:%M:%SZ")


def test_a_scheduled_renewal_charges_the_subscriber_once(
    store, subscription_worker, providers, well_stocked, settle_timeout
):
    slot = occurrence()
    authorizations_before = providers.count(P.AUTHORIZE)

    renew(subscription_worker, slot).unwrap()

    providers.await_call(P.AUTHORIZE, minimum=authorizations_before + 1)
    settled = until(
        lambda: [
            r for r in renewals(subscription_worker)
            if r["cron_occurrence"].startswith(slot[:16])
        ],
        lambda rows: bool(rows) and rows[0]["status"] == "confirmed",
        timeout=settle_timeout,
        description="the renewal to be confirmed",
    )[0]

    assert settled["status"] == "confirmed"
    assert providers.count(P.AUTHORIZE) == authorizations_before + 1, (
        "one renewal, one authorization"
    )
    # The renewal bills the subscription's own line total, not a cart.
    authorized = providers.last_call(P.AUTHORIZE)
    assert authorized["body"]["amount_minor"] == 2499


def test_replaying_the_same_occurrence_does_not_charge_twice(
    subscription_worker, providers, well_stocked, settle_timeout
):
    slot = occurrence()
    renew(subscription_worker, slot).unwrap()
    providers.await_call(P.AUTHORIZE)
    charged = providers.count(P.AUTHORIZE)

    renew(subscription_worker, slot).unwrap()

    assert providers.count(P.AUTHORIZE) == charged, (
        "a cron that fires twice for one occurrence must not bill twice"
    )


def test_a_shopper_cannot_trigger_their_own_renewal(shopper, providers):
    refused = renew(shopper, occurrence())

    assert refused.error_code() == "validation-failed"
    assert refused.error_message() == (
        "field 'start_subscription_renewal' not found in type: 'mutation_root'"
    )


def test_a_subscriber_can_read_only_their_own_subscription(shopper, other_shopper):
    mine = shopper.query("query { subscription { id customer_id } }")["subscription"]
    theirs = other_shopper.query("query { subscription { id customer_id } }")["subscription"]

    assert all(row["customer_id"] == d.CUSTOMER_ONE for row in mine)
    assert all(row["customer_id"] == d.CUSTOMER_TWO for row in theirs)
