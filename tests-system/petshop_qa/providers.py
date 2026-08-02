"""The store's five external providers, as seen from the test's side.

The stand answers payment, tax, carrier, payout and notification calls with the
example's own success fixture. Through its control plane a test can steer one
answer — a decline, a 5xx, a slow reply — and read back exactly what the store
sent, which is the only place a black-box test can check the money leaving.
"""

from __future__ import annotations

from typing import Any, Mapping

import requests

from .wait import until

# The operation paths the Petshop connectors are declared against. Regex, so a
# templated path matches any identifier the engine minted.
TAX_QUOTE = r"/v1/tax-quotes"
AUTHORIZE = r"/v1/payment-authorizations"
CAPTURE = r"/v1/payment-authorizations/[^/]+/captures"
VOID = r"/v1/payment-authorizations/[^/]+/voids"
REFUND = r"/v1/payment-authorizations/[^/]+/refunds"
RECONCILE = r"/v1/payment-reconciliations"
LOOKUP = r"/v1/payment-operation-lookups"
CAPTURE_LOOKUP = r"/v1/payment-capture-lookups"
SHIPMENT_LABEL = r"/v1/shipments/[^/]+/labels"
RETURN_LABEL = r"/v1/returns/[^/]+/labels"
PAYOUT = r"/v1/payouts"


class Providers:
    """Client for the mock providers' test-only control plane."""

    def __init__(self, base_url: str, *, request_timeout: float = 15, settle_timeout: float = 30):
        self.base_url = base_url.rstrip("/")
        self._request_timeout = request_timeout
        self._settle_timeout = settle_timeout
        self._session = requests.Session()

    # -- steering ----------------------------------------------------------

    def script(
        self,
        path: str,
        *,
        status: int = 200,
        body: Any = None,
        patch: Mapping[str, Any] | None = None,
        when: Mapping[str, Any] | None = None,
        delay_ms: int = 0,
        times: int = 1,
    ) -> None:
        """Steer the next `times` calls to `path`.

        `patch` merges into the default success answer, which keeps the echoed
        identifiers a Process asserts on intact while one field is changed.
        """

        spec: dict[str, Any] = {"path": path, "status": status, "delay_ms": delay_ms, "times": times}
        if body is not None:
            spec["body"] = body
        if patch:
            spec["patch"] = dict(patch)
        if when:
            spec["when"] = dict(when)
        response = self._session.post(
            f"{self.base_url}/_control/script", json=spec, timeout=self._request_timeout
        )
        response.raise_for_status()

    def decline_authorization(self, *, times: int = 1) -> None:
        """The provider refuses the card: a successful call with a refusal."""

        self.script(AUTHORIZE, patch={"status": "declined"}, times=times)

    def fail(self, path: str, *, status: int = 500, times: int = 1) -> None:
        """The provider is unavailable — retryable for the declared classes."""

        self.script(path, status=status, body={"error": "provider unavailable"}, times=times)

    def hang(self, path: str, *, delay_ms: int = 6000, times: int = 1) -> None:
        """The provider is too slow, so the activity's own timeout decides."""

        self.script(path, delay_ms=delay_ms, times=times)

    def reset(self) -> None:
        """Drop every script and the journal. Runs between tests."""

        response = self._session.post(f"{self.base_url}/_control/reset", timeout=self._request_timeout)
        response.raise_for_status()

    # -- observing ---------------------------------------------------------

    def calls(self, path: str | None = None) -> list[dict]:
        params = {"path": path} if path else None
        response = self._session.get(
            f"{self.base_url}/_control/calls", params=params, timeout=self._request_timeout
        )
        response.raise_for_status()
        return response.json()["calls"]

    def count(self, path: str) -> int:
        return len(self.calls(path))

    def last_call(self, path: str) -> dict:
        calls = self.calls(path)
        assert calls, f"the store never called {path}"
        return calls[-1]

    def await_call(self, path: str, *, minimum: int = 1, timeout: float | None = None) -> list[dict]:
        """Wait until the store has called `path` at least `minimum` times."""

        return until(
            lambda: self.calls(path),
            lambda calls: len(calls) >= minimum,
            timeout=timeout if timeout is not None else self._settle_timeout,
            description=f"{minimum} call(s) to {path}",
        )

    def pending_scripts(self) -> list[dict]:
        response = self._session.get(
            f"{self.base_url}/_control/scripts", timeout=self._request_timeout
        )
        response.raise_for_status()
        return response.json()["scripts"]

    def is_up(self) -> bool:
        try:
            response = self._session.get(f"{self.base_url}/", timeout=self._request_timeout)
        except requests.RequestException:
            return False
        return response.status_code == 200

    def control_enabled(self) -> bool:
        try:
            response = self._session.get(f"{self.base_url}/", timeout=self._request_timeout)
            return bool(response.json().get("control"))
        except (requests.RequestException, ValueError):
            return False
