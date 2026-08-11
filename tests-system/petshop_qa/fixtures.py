"""Fixtures for any suite that drives a Petshop stand over HTTP.

The suite is pointed at a running store with PETSHOP_BASE_URL. Without it every
test skips: a run with no stand must not look like a passing run, and must not
look like a failing store either.

These live in the package rather than in a `conftest.py` because two suites
now use them: the black-box system tests in `tests-system/tests`, and the eval
scenarios in `evals/tasks/*/scenarios`, which drive the same store built by
somebody else. Load them with `pytest_plugins = ["petshop_qa.fixtures"]`.
"""

from __future__ import annotations

import os

import pytest

from petshop_qa import Config, Providers, Store
from petshop_qa.domain import CUSTOMER_ONE, CUSTOMER_TWO


def pytest_report_header(config):  # pragma: no cover - reporting only
    resolved = Config.from_env()
    if resolved is None:
        return "petshop: no stand configured (set PETSHOP_BASE_URL)"
    providers = resolved.providers_url or "not configured"
    return f"petshop: {resolved.base_url} (providers: {providers})"


@pytest.fixture(scope="session")
def config() -> Config:
    resolved = Config.from_env()
    if resolved is None:
        pytest.skip(
            "no Petshop stand configured: raise one with tests-system/stack.sh up "
            "and export PETSHOP_BASE_URL"
        )
    return resolved


@pytest.fixture(scope="session")
def store(config: Config) -> Store:
    store = Store(config)
    if not store.is_up():
        pytest.fail(
            f"no store answering at {config.base_url}; "
            "raise one with tests-system/stack.sh up"
        )
    return store


@pytest.fixture
def providers(request, config: Config) -> Providers:
    """The mock providers' control plane, cleaned before and after each test.

    Scripted answers are stand-wide state, so a test that steers a provider
    must not run beside another one: mark it `serial` and run the suite with
    one worker (the default).
    """

    if config.providers_url is None:
        pytest.skip("provider steering needs PETSHOP_PROVIDERS_URL")
    control = Providers(
        config.providers_url,
        request_timeout=config.request_timeout,
        settle_timeout=config.settle_timeout,
    )
    if not control.is_up():
        pytest.fail(f"no mock providers answering at {config.providers_url}")
    if not control.control_enabled():
        pytest.skip(
            "the mock providers run without their control plane "
            "(set PETSHOP_PROVIDERS_CONTROL=1 on the stand)"
        )
    # The store applies several transitions at once, so an order started by the
    # previous scenario can still be on its way to a provider. Steering one
    # before that work is done hands this test's scripted answer to the last
    # test's order — and reads its call in this test's journal.
    control.await_quiet(seconds=1.5)
    control.reset()
    yield control
    control.reset()


# -- the people using the store ---------------------------------------------


@pytest.fixture
def anonymous(store: Store):
    """A visitor with no token at all — the engine's unauthorized role."""

    return store.anonymous()


@pytest.fixture
def shopper(store: Store):
    return store.as_role("customer", CUSTOMER_ONE)


@pytest.fixture
def other_shopper(store: Store):
    """A second, unrelated customer: the one whose data must stay invisible."""

    return store.as_role("customer", CUSTOMER_TWO)


@pytest.fixture
def staff(store: Store):
    return store.as_role("staff")


@pytest.fixture
def support(store: Store):
    return store.as_role("support")


@pytest.fixture
def fulfilment(store: Store):
    return store.as_role("fulfilment")


@pytest.fixture
def payment_worker(store: Store):
    return store.as_role("payment_worker")


@pytest.fixture
def settle_timeout(config: Config) -> float:
    return config.settle_timeout


@pytest.fixture
def well_stocked(staff):
    """Enough inventory for the scenario to be about the scenario."""

    from petshop_qa import domain as d

    d.ensure_stock(staff, d.IN_STOCK_VARIANT)
    return staff


@pytest.fixture
def subscription_worker(store: Store):
    return store.as_role("subscription_worker")


@pytest.fixture
def marketplace_worker(store: Store):
    return store.as_role("marketplace_worker")


@pytest.fixture
def reviewer(store: Store):
    """The veterinarian who decides on a prescription."""

    return store.as_role("veterinary_reviewer", "veterinarian-1")


@pytest.fixture
def reconciliation_worker(store: Store):
    return store.as_role("reconciliation_worker")


@pytest.fixture
def b2b_buyer(store: Store):
    """A buyer purchasing on behalf of the provisioned organization."""

    return store.as_role("b2b_buyer", CUSTOMER_ONE)


@pytest.fixture
def b2b_approver(store: Store):
    return store.as_role("b2b_approver", "approver-1")


@pytest.fixture
def b2b_finance(store: Store):
    return store.as_role("b2b_finance", "finance-1")


# -- the stand whose clocks run in seconds ----------------------------------


@pytest.fixture(scope="session")
def fast_config() -> Config:
    """The second stand, serving the same store with shortened periods.

    Deadlines and dunning delays ship as days, so the branches behind them are
    unreachable on the ordinary stand. `stack.sh up-fast` raises a stand that
    declares the same flows in seconds; without it these cases skip rather than
    pretend the branches are covered.
    """

    base_url = os.environ.get("PETSHOP_FAST_BASE_URL", "").strip()
    if not base_url:
        pytest.skip(
            "time-based branches need the fast stand: tests-system/stack.sh up-fast "
            "and export PETSHOP_FAST_BASE_URL"
        )
    resolved = Config.from_env()
    assert resolved is not None
    return Config(
        base_url=base_url.rstrip("/"),
        jwt_key=resolved.jwt_key,
        jwt_algorithm=resolved.jwt_algorithm,
        providers_url=os.environ.get("PETSHOP_FAST_PROVIDERS_URL", resolved.providers_url or ""),
        request_timeout=resolved.request_timeout,
        settle_timeout=float(os.environ.get("PETSHOP_FAST_SETTLE_TIMEOUT", "45")),
    )


@pytest.fixture
def fast_store(fast_config: Config) -> Store:
    store = Store(fast_config)
    if not store.is_up():
        pytest.fail(f"no fast stand answering at {fast_config.base_url}")
    return store


@pytest.fixture
def fast_settle_timeout(fast_config: Config) -> float:
    return fast_config.settle_timeout


@pytest.fixture
def fast_providers(fast_config: Config) -> Providers:
    """The fast stand's own providers.

    Separate from the ordinary stand's on purpose: one shared instance let a
    scenario's scripted answer be claimed by the other stand's durable work.
    """

    url = os.environ.get("PETSHOP_FAST_PROVIDERS_URL", "").strip()
    if not url:
        pytest.skip("the fast stand's providers need PETSHOP_FAST_PROVIDERS_URL")
    control = Providers(
        url.rstrip("/"),
        request_timeout=fast_config.request_timeout,
        settle_timeout=fast_config.settle_timeout,
    )
    if not control.is_up():
        pytest.fail(f"no fast-stand providers answering at {url}")
    # The fast stand's deadlines are seconds, so a previous scenario's ladder is
    # often still climbing when the next one starts. Steering a provider before
    # that work is done hands this test's scripted answers to the last one.
    # Longer than the fast stand's dunning delay, or a ladder still waiting
    # between its rungs reads as finished and its next call lands in the next
    # scenario's journal.
    control.await_quiet(seconds=4.0)
    control.reset()
    yield control
    control.reset()
