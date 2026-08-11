"""Does this store's account of the money agree with the provider's?

Every scenario is a world the payment provider is put into, an action a shopper
takes, and an outcome that must hold afterwards. Nothing here names a state, a
command or a table: the candidate chose those, and a different correct design
would have chosen others. What it cannot choose is what the shopper and support
end up seeing, and whether the money is where the store says it is.
"""

from __future__ import annotations

import uuid

import pytest

from petshop_qa import domain as d
from petshop_qa import providers as P
from petshop_qa import stays

pytestmark = [pytest.mark.providers, pytest.mark.serial]

#: Long enough for three timed-out attempts and the work that follows them.
SILENCE_MS = 9000


# -- the worlds --------------------------------------------------------------


def provider_authorizes(providers) -> None:
    """The ordinary answer: the card is good and the provider says so."""


def provider_fails_then_authorizes(providers) -> None:
    """One 5xx, then the provider recovers."""

    providers.fail(P.AUTHORIZE, status=500, times=1)


def provider_declines(providers) -> None:
    providers.decline_authorization()


def provider_goes_silent(providers) -> None:
    """The call never gets an answer — whether the card was charged is unknown."""

    providers.hang(P.AUTHORIZE, delay_ms=SILENCE_MS, times=3)


def lookup_says(providers, **patch) -> None:
    """The provider's own account of an operation it was asked about."""

    providers.script(P.LOOKUP, times=10, patch=patch)


def provider_times_out_after_charging(providers, *, amount_minor: int) -> None:
    """It took the money, then went quiet — and will say so when asked."""

    lookup_says(
        providers,
        found=True,
        terminal_absence_proven=False,
        outcome="authorized",
        provider_mutation_id=f"mutation_{uuid.uuid4().hex[:8]}",
        provider_event_id=f"evt_{uuid.uuid4().hex[:8]}",
        provider_reference=f"ref_{uuid.uuid4().hex[:8]}",
        amount_minor=amount_minor,
    )


def provider_cannot_prove_absence(providers) -> None:
    """It has no record of the operation — and cannot swear it never happened."""

    lookup_says(providers, found=False, terminal_absence_proven=False)


def provider_challenges_the_card(providers) -> None:
    """A live answer the store has no obvious case for: the shopper must act."""

    providers.script(P.AUTHORIZE, times=1, patch={"status": "challenged"})


# -- what a shopper does -----------------------------------------------------


def checkout(shopper, timeout: float) -> dict:
    known = {order["id"] for order in d.orders_of(shopper)}
    d.start_checkout(shopper, d.cart_with_one_line(shopper)).unwrap()
    return d.await_new_order(shopper, known=known, timeout=timeout)


def order_status(shopper, order_id: str) -> str | None:
    for order in d.orders_of(shopper):
        if order["id"] == order_id:
            return order["order_status"]
    return None


def money_held(support, order_id: str) -> bool:
    return any(p["status"] == "authorized" for p in d.payments_of(support, order_id))


def charged_once(providers, order_id: str) -> None:
    """One charge, however many calls it took to be sure of it."""

    calls = providers.calls_about(P.AUTHORIZE, order_id=order_id)
    keys = {call["headers"].get("idempotency-key") for call in calls}
    assert keys and None not in keys, (
        "every authorization attempt carries the key the provider deduplicates on"
    )
    assert len(keys) == 1, (
        f"the provider was asked to charge {len(keys)} different times for one order"
    )


# -- the provider answers ----------------------------------------------------


def test_a_paid_order_is_authorized(
    shopper, support, providers, well_stocked, settle_timeout
):
    provider_authorizes(providers)

    order = checkout(shopper, settle_timeout)

    d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)
    payment = d.await_payment_status(
        support, order["id"], {"authorized"}, timeout=settle_timeout
    )
    assert payment["amount_minor"] == d.total_of(order), (
        "the money held is the money the order asked for"
    )
    charged_once(providers, order["id"])


def test_a_transient_failure_does_not_lose_the_sale(
    shopper, support, providers, well_stocked, settle_timeout
):
    provider_fails_then_authorizes(providers)

    order = checkout(shopper, settle_timeout)

    d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)
    d.await_payment_status(support, order["id"], {"authorized"}, timeout=settle_timeout)
    assert len(providers.calls_about(P.AUTHORIZE, order_id=order["id"])) >= 2, (
        "a provider that answered 500 once was asked again"
    )
    charged_once(providers, order["id"])


def test_a_declined_card_takes_no_money_and_puts_the_stock_back(
    shopper, support, staff, providers, well_stocked, settle_timeout
):
    before = d.stock(staff, d.IN_STOCK_VARIANT)
    provider_declines(providers)

    order = checkout(shopper, settle_timeout)

    d.await_order_status(shopper, order["id"], {"cancelled"}, timeout=settle_timeout)
    assert not money_held(support, order["id"]), "a refused card holds no money"
    after = d.stock(staff, d.IN_STOCK_VARIANT)
    assert after["reserved"] == before["reserved"], (
        "a refused checkout must not keep holding the shelf"
    )


# -- the provider goes quiet -------------------------------------------------


def test_a_proven_absence_takes_no_money(
    shopper, support, providers, well_stocked, settle_timeout
):
    """The call timed out, and the provider proves the card was never charged."""

    provider_goes_silent(providers)

    order = checkout(shopper, settle_timeout)

    stays(
        lambda: money_held(support, order["id"]),
        lambda held: not held,
        duration=min(10.0, settle_timeout),
        description="a charge the provider says never happened",
    )
    assert order_status(shopper, order["id"]) != "authorized", (
        "an order nobody paid for is not authorized"
    )


def test_a_charge_the_store_never_learned_about(
    shopper, support, providers, well_stocked, settle_timeout
):
    """The provider took the money and went quiet. The store must find out.

    Giving up here — leaving the checkout for a person, or failing it — loses a
    charge the provider is perfectly willing to describe when asked.
    """

    provider_goes_silent(providers)
    order = checkout(shopper, settle_timeout)
    provider_times_out_after_charging(providers, amount_minor=d.total_of(order))

    d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)
    payment = d.await_payment_status(
        support, order["id"], {"authorized"}, timeout=settle_timeout
    )
    assert payment["amount_minor"] == d.total_of(order), (
        "the store records the amount the provider says it took"
    )


def test_an_ambiguous_charge_is_investigated_not_written_off(
    shopper, providers, well_stocked, settle_timeout
):
    """Before it decides anything, the store asks the provider what happened.

    A second, independent way of catching a store that gives up on silence: the
    first scenario reads the store's own account of the money, this one reads
    the provider's call log. A store that writes an ambiguous charge off never
    asks, and no amount of internal bookkeeping hides that.

    Nothing here waits on the order first. An earlier version opened with the
    same `await_order_status(..., authorized)` as the scenario above, which
    meant both died on that one line and the call-log assertion never ran — two
    scenarios resting on a single reading, which is the thing the two-killer
    rule exists to prevent. Asking the provider is observable on its own, and
    it is what this scenario is for.
    """

    provider_goes_silent(providers)
    order = checkout(shopper, settle_timeout)
    provider_times_out_after_charging(providers, amount_minor=d.total_of(order))

    providers.await_call(P.LOOKUP, timeout=settle_timeout)


def test_an_unproven_absence_is_not_a_decline(
    shopper, providers, well_stocked, settle_timeout
):
    """"I have no record" is not "it did not happen" unless the provider says so."""

    provider_goes_silent(providers)
    order = checkout(shopper, settle_timeout)
    provider_cannot_prove_absence(providers)

    stays(
        lambda: order_status(shopper, order["id"]),
        lambda status: status not in {"cancelled", "declined"},
        duration=min(12.0, settle_timeout),
        description="a checkout cancelled on an absence nobody proved",
    )


def test_no_refusal_is_recorded_that_the_provider_never_gave(
    shopper, support, providers, well_stocked, settle_timeout
):
    """The books' half of the same question, on its own.

    Kept apart from the shopper's view deliberately: two scenarios that can
    each fail alone survive a change that quietly weakens one of them, and a
    task whose discrimination hangs on a single assertion has none the day that
    assertion drifts.

    Support reads the payments. "The provider refused" is a claim about what
    the provider said, and here the provider said only that it cannot find the
    operation — which is not the same sentence.
    """

    provider_goes_silent(providers)
    order = checkout(shopper, settle_timeout)
    provider_cannot_prove_absence(providers)

    refused = {"declined", "failed", "refused"}
    stays(
        lambda: [p["status"] for p in d.payments_of(support, order["id"])],
        lambda seen: not (refused & set(seen)),
        duration=min(12.0, settle_timeout),
        description="a refusal recorded against a provider that never gave one",
    )


# -- the provider answers something else -------------------------------------


def test_an_authorized_order_has_money_behind_it(
    shopper, providers, well_stocked, settle_timeout
):
    """The provider challenged the card: it is holding nothing yet.

    What the shopper is told, on its own.
    """

    provider_challenges_the_card(providers)

    order = checkout(shopper, settle_timeout)

    stays(
        lambda: order_status(shopper, order["id"]),
        lambda status: status != "authorized",
        duration=min(12.0, settle_timeout),
        description="an order authorized on an outcome the provider never gave",
    )


def test_a_challenged_card_is_not_money_in_the_till(
    shopper, support, providers, well_stocked, settle_timeout
):
    """And what the books say, on its own.

    Support reads the payments, the shopper reads the order; a store that
    mistakes a challenge for a success is wrong in both places, and either
    reading catches it without the other.
    """

    provider_challenges_the_card(providers)

    order = checkout(shopper, settle_timeout)

    stays(
        lambda: money_held(support, order["id"]),
        lambda held: not held,
        duration=min(12.0, settle_timeout),
        description="the books hold money the provider never authorized",
    )
