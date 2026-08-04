"""The dunning ladder when the provider will not answer at all.

Each rung of a failed renewal has the same shape as the first: ask, wait for
an answer that does not come, look the operation up, and act on what the
provider says happened. Walking the whole ladder that way is the only path
through the store's largest flow, and it ends either renewed or paused.

Runs on the fast stand: the delays between rungs ship as days.
"""

from __future__ import annotations

import uuid

import pytest

from petshop_qa import domain as d
from petshop_qa import providers as P
from petshop_qa import until

pytestmark = [pytest.mark.providers, pytest.mark.serial]

SUBSCRIPTION = "00000000-0000-0000-0000-0000000000d1"


def occurrence() -> str:
    """A schedule slot no other renewal has used.

    Down to the second: a repeated slot is the same occurrence, and the store
    answers a replay with the first renewal's outcome rather than a new one.
    """

    token = uuid.uuid4().int
    return (
        f"2035-{token % 12 + 1:02d}-{token // 12 % 28 + 1:02d}"
        f"T{token // 400 % 24:02d}:{token // 10_000 % 60:02d}:{token // 1_000_000 % 60:02d}Z"
    )


def silent_provider(stand_providers, *, attempts: int = 24) -> None:
    """Every authorization attempt times out rather than answering.

    Generously many: the ladder has three rungs and each retries, so a stand-in
    that ran out of silence half-way would let a late attempt succeed and the
    test would be about something else.
    """

    stand_providers.hang(P.AUTHORIZE, delay_ms=9000, times=attempts)


def lookup_says(stand_providers, outcome: str, *, times: int = 24) -> None:
    """Every rung's lookup answers the same way — with its own event id.

    The store files each provider event under the id the provider gave it and
    keys the recording command on that id, so a stand-in that repeated one id
    across rungs would look like the same operation reported twice. Leaving
    the id to the mock is what makes each rung its own event.
    """

    # One scripted answer per call, each with its own identifiers. A provider
    # gives every operation its own event and authorization id, and the store
    # files them under unique indexes — repeating one across rungs would look
    # like the same authorization reported twice, which the store refuses.
    for _ in range(times):
        stand_providers.script(
            P.LOOKUP,
            times=1,
            patch={
                "found": True,
                "terminal_absence_proven": False,
                "outcome": outcome,
                "amount_minor": 2499,
                "provider_mutation_id": f"auth_{uuid.uuid4().hex[:10]}",
                "provider_event_id": f"evt_{uuid.uuid4().hex[:10]}",
                "provider_reference": f"ref_{uuid.uuid4().hex[:10]}",
            },
        )


def renew(worker, slot: str):
    """Start one renewal, from an active plan.

    The plan is put back to active here rather than in a fixture: starting a
    renewal is what moves it to `payment_due`, so anything between the reset
    and the call is a window in which the previous scenario can take it away.
    """

    restore_the_plan(worker)
    return worker.graphql(
        """
        mutation Renew($subscription: uuid!, $occurrence: timestamptz!) {
          start_subscription_renewal(
            subscription_id: $subscription, cron_occurrence: $occurrence
          ) { subscription_id }
        }
        """,
        {"subscription": SUBSCRIPTION, "occurrence": slot},
    )


def renewal_at(worker, slot: str, settled: set[str], timeout: float) -> dict:
    return until(
        lambda: worker.query(
            """
            query R($occurrence: timestamptz!) {
              subscription_renewal(where: {cron_occurrence: {_eq: $occurrence}}) {
                id status
              }
            }
            """,
            {"occurrence": slot},
        )["subscription_renewal"],
        lambda rows: bool(rows) and rows[0]["status"] in settled,
        timeout=timeout,
        description=f"the renewal at {slot} to reach {sorted(settled)}",
    )[0]


@pytest.fixture(autouse=True)
def an_active_plan(fast_store, fast_providers):
    """The stand carries one subscription, and a ladder leaves it stopped.

    Reset before rather than after: a scenario that fails half-way would
    otherwise leave the next one renewing a plan the store has already paused,
    and every later failure would be about that instead of about the flow.
    """

    worker = fast_store.as_role("subscription_worker")
    yield


def restore_the_plan(worker) -> None:
    """Put the provisioned subscription back to active."""

    worker.graphql(
        """
        mutation Resume($id: uuid!) {
          update_subscription(where: {id: {_eq: $id}}, _set: {status: "active"}) {
            affected_rows
          }
        }
        """,
        {"id": SUBSCRIPTION},
    )


def test_a_ladder_of_unanswered_attempts_ends_with_a_paused_plan(
    fast_store, fast_providers, fast_settle_timeout
):
    """Nobody ever answers, and every lookup says the card was refused.

    This walks every rung: the initial attempt, both dunning retries, and the
    reconciliation behind each of them — the longest path the store has.
    """

    worker = fast_store.as_role("subscription_worker")
    slot = occurrence()
    silent_provider(fast_providers)
    lookup_says(fast_providers, "declined")

    renew(worker, slot).unwrap()

    # The renewal row records the attempt — `dunning` once the card has been
    # refused — while the plan itself is what the ladder ends up stopping.
    settled = renewal_at(worker, slot, {"dunning", "failed", "cancelled"}, fast_settle_timeout * 2)
    assert settled["status"] == "dunning", f"the renewal records the refusals: {settled}"

    plan = until(
        lambda: worker.query(
            "query P($id: uuid!) { subscription(where: {id: {_eq: $id}}) { id status } }",
            {"id": SUBSCRIPTION},
        )["subscription"][0],
        lambda row: row["status"] in {"paused", "cancelled"},
        timeout=fast_settle_timeout,
        description="the plan to stop after the last rung",
    )
    assert plan["status"] == "paused", (
        f"a card refused at every rung pauses the plan rather than billing on: {plan}"
    )


def test_a_retry_the_provider_did_take_renews_the_plan(
    fast_store, fast_providers, fast_settle_timeout
):
    """The retry's call times out, and the lookup says it went through."""

    worker = fast_store.as_role("subscription_worker")
    slot = occurrence()
    silent_provider(fast_providers)
    lookup_says(fast_providers, "authorized")

    renew(worker, slot).unwrap()

    settled = renewal_at(worker, slot, {"confirmed", "failed", "paused"}, fast_settle_timeout)
    assert settled["status"] == "confirmed", (
        f"a renewal the provider says it took is confirmed: {settled}"
    )
    restore_the_plan(worker)


def test_an_answer_the_provider_will_not_give_stops_the_renewal(
    fast_store, fast_providers, fast_settle_timeout
):
    """Neither found nor proven absent: the store does not decide for itself."""

    worker = fast_store.as_role("subscription_worker")
    slot = occurrence()
    silent_provider(fast_providers)
    fast_providers.script(
        P.LOOKUP,
        times=12,
        patch={"found": False, "terminal_absence_proven": False, "outcome": "failed"},
    )

    renew(worker, slot).unwrap()

    fast_providers.await_call(P.LOOKUP, timeout=fast_settle_timeout)
    rows = worker.query(
        """
        query R($occurrence: timestamptz!) {
          subscription_renewal(where: {cron_occurrence: {_eq: $occurrence}}) { id status }
        }
        """,
        {"occurrence": slot},
    )["subscription_renewal"]
    assert all(row["status"] != "confirmed" for row in rows), (
        f"an undetermined answer never becomes a renewal: {rows}"
    )
    restore_the_plan(worker)


def test_a_retry_the_provider_took_after_a_refusal_renews_the_plan(
    fast_store, fast_providers, fast_settle_timeout
):
    """The card is refused, then the retry's call gets no answer at all.

    The lookup behind that retry says the money was taken after all, so the
    renewal is recorded from what the provider says happened rather than from
    the answer the store never got — and the shopper is not billed twice.
    """

    worker = fast_store.as_role("subscription_worker")
    slot = occurrence()
    # One clean refusal, then silence: the scripts are used in the order they
    # are added, so the first attempt is the decline and every later call hangs.
    fast_providers.decline_authorization(times=1)
    silent_provider(fast_providers)
    lookup_says(fast_providers, "authorized")

    renew(worker, slot).unwrap()

    settled = renewal_at(worker, slot, {"confirmed", "failed", "cancelled"}, fast_settle_timeout)
    assert settled["status"] == "confirmed", (
        f"a retry the provider says it took renews the plan: {settled}"
    )
    restore_the_plan(worker)


def test_the_last_retry_the_provider_took_still_renews_the_plan(
    fast_store, fast_providers, fast_settle_timeout
):
    """Refused twice, and the third attempt's answer never arrives.

    This is the last rung of the ladder: had the lookup not found the
    authorization, the plan would have been paused. Reading the provider's own
    record is what keeps a paying customer from being cut off.
    """

    worker = fast_store.as_role("subscription_worker")
    slot = occurrence()
    fast_providers.decline_authorization(times=2)
    silent_provider(fast_providers)
    lookup_says(fast_providers, "authorized")

    renew(worker, slot).unwrap()

    settled = renewal_at(
        worker, slot, {"confirmed", "failed", "cancelled"}, fast_settle_timeout * 2
    )
    assert settled["status"] == "confirmed", (
        f"the last rung is settled from the provider's record: {settled}"
    )
    restore_the_plan(worker)


def test_a_card_refused_at_every_rung_pauses_the_plan_without_a_lookup(
    fast_store, fast_providers, fast_settle_timeout
):
    """Three clear refusals, no ambiguity, nothing to reconcile.

    Every attempt is answered — with a no — so the store never asks the
    provider what happened. It walks the whole ladder and stops billing.
    """

    worker = fast_store.as_role("subscription_worker")
    slot = occurrence()
    fast_providers.decline_authorization(times=6)

    renew(worker, slot).unwrap()

    plan = until(
        lambda: worker.query(
            "query P($id: uuid!) { subscription(where: {id: {_eq: $id}}) { id status } }",
            {"id": SUBSCRIPTION},
        )["subscription"][0],
        lambda row: row["status"] in {"paused", "cancelled"},
        timeout=fast_settle_timeout * 2,
        description="the plan to stop after three refusals",
    )
    assert plan["status"] == "paused", f"a card that never works pauses the plan: {plan}"
    assert fast_providers.count(P.LOOKUP) == 0, (
        "an answered refusal is not something the store asks about again"
    )
    # Three attempts and no more: the ladder has a top.
    assert fast_providers.count(P.AUTHORIZE) == 3, (
        f"the ladder is three rungs: {fast_providers.count(P.AUTHORIZE)} authorizations"
    )
    restore_the_plan(worker)
