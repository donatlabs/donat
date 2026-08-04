"""Stand-in payment, tax, carrier, payout and notification providers.

The Petshop connectors are declared against five external HTTP services. This
answers them so the example runs end to end without an account anywhere. It is
a fixture, not a simulator: every answer is the success shape its operation
declares, and identifiers the caller sent are echoed back, because a Process
asserts that an answer belongs to the request it was given.

The engine treats an answer as untrusted input and validates it against the
operation's declared response contract, so an answer that drifts from the
declaration fails the activity instead of propagating.

With PETSHOP_PROVIDERS_CONTROL=1 it also exposes a test-only control plane
under /_control (see CONTROL_HELP): black-box system tests script a decline, a
5xx, or a slow answer for the next call to an operation, and read back the
journal of what the store actually sent. Never enable it outside testing —
default behaviour, with nothing scripted, is exactly the success fixture above.
"""

import json
import os
import re
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

PORT = 8099

CONTROL_ENABLED = os.environ.get("PETSHOP_PROVIDERS_CONTROL") == "1"

#: How many calls the journal keeps. Bounded so a long-lived stand cannot grow
#: without limit; a test reads its own calls long before they age out.
JOURNAL_LIMIT = 500

CONTROL_HELP = {
    "POST /_control/reset": "drop every scripted response and the call journal",
    "POST /_control/script": {
        "path": "regex matched against the request path, e.g. /v1/payment-authorizations",
        "when": "optional {body_field: value} — scopes the script to one order/payment",
        "status": "HTTP status to answer with (default 200)",
        "body": "literal JSON body; omit to keep the default success answer",
        "patch": "fields merged into the default answer, e.g. {'status': 'declined'}",
        "delay_ms": "hold the answer this long, to drive a client-side timeout",
        "times": "how many matching calls it applies to (default 1)",
    },
    "GET /_control/calls?path=<regex>": "the recorded calls, newest last",
    "GET /_control/scripts": "scripted responses still waiting to be used",
}

EVIDENCE = {"provider": "mock", "example": "petshop"}

_IDS = {}
_IDS_LOCK = threading.Lock()


def issued(kind, key):
    """An identifier this provider issues once per operation.

    A provider gives every operation its own event id, and gives a *replay* of
    the same request the same one back — that is what an idempotency key is
    for. The store relies on both: it stores provider event ids under a unique
    index, so a fixture that answered with a constant would let the second
    order in a stand collide with the first.
    """

    with _IDS_LOCK:
        existing = _IDS.get((kind, key))
        if existing is None:
            existing = f"{kind}_{len(_IDS) + 1}_{uuid.uuid4().hex[:8]}"
            _IDS[(kind, key)] = existing
        return existing


def route(path, body, key):
    """The success answer for one operation, or None if nothing declares it.

    `key` identifies the caller's attempt: the Idempotency-Key header when the
    operation carries one, otherwise a value unique to this call.
    """

    # -- tax -----------------------------------------------------------------
    if path == "/v1/tax-quotes":
        return {
            "tax_quote_id": issued("tax_quote", key),
            "checkout_quote_id": body.get("checkout_quote_id"),
            "tax_minor": 160,
            "tax_code": "US-STANDARD",
            "currency": body.get("currency", "USD"),
        }

    # -- payment -------------------------------------------------------------
    if path == "/v1/payment-authorizations":
        return {
            "provider_event_id": issued("evt_authorize", key),
            "authorization_id": issued("auth", key),
            "provider_reference": issued("ref", key),
            "status": "authorized",
            "normalized_payload": EVIDENCE,
        }
    if re.fullmatch(r"/v1/payment-authorizations/[^/]+/captures", path):
        return {
            "payment_id": body.get("payment_id"),
            "shipment_id": body.get("shipment_id"),
            "amount_minor": body.get("amount_minor"),
            "provider_event_id": issued("evt_capture", key),
            "capture_id": issued("cap", key),
            "provider_reference": issued("ref", key),
            "status": "captured",
            "normalized_payload": EVIDENCE,
        }
    if re.fullmatch(r"/v1/payment-authorizations/[^/]+/voids", path):
        return {
            "provider_event_id": issued("evt_void", key),
            "void_id": issued("void", key),
            "provider_reference": issued("ref", key),
            "status": "voided",
            "normalized_payload": EVIDENCE,
        }
    if re.fullmatch(r"/v1/payment-authorizations/[^/]+/refunds", path):
        return {
            "provider_event_id": issued("evt_refund", key),
            "refund_id": issued("refund", key),
            "provider_reference": issued("ref", key),
            "status": "refunded",
            "normalized_payload": EVIDENCE,
        }
    if path == "/v1/payment-reconciliations":
        # Agreeing with the store on every compared fact is what makes a
        # reconciliation match automatically instead of waiting for support.
        return {
            "provider_event_id": body.get("provider_event_id"),
            "reconciliation_id": issued("recon", key),
            "amount_minor": body.get("expected_amount_minor", 0),
            "currency": "USD",
            "status": "authorized",
            "provider_reference": body.get("provider_reference"),
            "normalized_payload": EVIDENCE,
        }
    if path in ("/v1/payment-operation-lookups", "/v1/payment-capture-lookups"):
        # An in-doubt mutation that the provider proves never happened. The
        # contract is non-null throughout, so an absence still answers every
        # declared field.
        return {
            "found": False,
            "terminal_absence_proven": True,
            "provider_event_id": issued("evt_lookup_absent", key),
            "provider_mutation_id": "",
            "provider_reference": "",
            "outcome": "failed",
            "amount_minor": 0,
            "currency": "USD",
            "captured": [],
            "terminal_absences": [],
            "normalized_payload": EVIDENCE,
        }

    # -- carrier -------------------------------------------------------------
    if re.fullmatch(r"/v1/shipments/[^/]+/labels", path):
        return {
            "shipment_id": body.get("shipment_id"),
            "shipment_key": body.get("shipment_key"),
            "carrier_shipment_reference": issued("carrier_ref", key),
            "tracking_number": issued("TRACK", key),
            "label_url": "https://carrier.example/labels/1.pdf",
            "event_id": issued("evt_label", key),
            "outcome": "label_created",
        }
    if re.fullmatch(r"/v1/returns/[^/]+/labels", path):
        return {
            "provider_event_id": issued("evt_return_label", key),
            "return_id": body.get("return_key"),
            "tracking_number": issued("RETURN-TRACK", key),
            "label_url": "https://carrier.example/returns/1.pdf",
            "status": "created",
            "normalized_payload": EVIDENCE,
        }

    # -- payout --------------------------------------------------------------
    if path == "/v1/payouts":
        return {
            "local_payout_id": body.get("payout_id"),
            "vendor_id": body.get("vendor_id"),
            "provider_event_id": issued("evt_payout", key),
            "payout_id": issued("payout", key),
            "outcome": "paid",
            "normalized_payload": EVIDENCE,
        }

    return None


class Control:
    """Scripted answers and the journal of what the store actually sent.

    A system test cannot know the identifiers the engine will mint — an order
    is created inside the durable Process, after the entry-point Command has
    already answered. So a script is registered before the run and claimed by
    the first matching call, rather than addressed to a known id.
    """

    def __init__(self):
        self._lock = threading.Lock()
        self._scripts = []
        self._journal = []

    def add_script(self, spec):
        script = {
            "path": spec.get("path", ".*"),
            "when": spec.get("when") or {},
            "status": int(spec.get("status", 200)),
            "body": spec.get("body"),
            "patch": spec.get("patch") or {},
            "delay_ms": int(spec.get("delay_ms", 0)),
            "times": int(spec.get("times", 1)),
        }
        with self._lock:
            self._scripts.append(script)
        return script

    def claim(self, path, body):
        """The script that applies to this call, consuming one of its uses."""

        with self._lock:
            for script in self._scripts:
                if not re.fullmatch(script["path"], path):
                    continue
                if any(body.get(key) != value for key, value in script["when"].items()):
                    continue
                script["times"] -= 1
                if script["times"] <= 0:
                    self._scripts.remove(script)
                return script
        return None

    def record(self, path, body, headers, status):
        entry = {
            "path": path,
            "body": body,
            "headers": {name.lower(): value for name, value in headers.items()},
            "status": status,
            "at": time.time(),
        }
        with self._lock:
            self._journal.append(entry)
            del self._journal[:-JOURNAL_LIMIT]

    def calls(self, path_pattern=None):
        with self._lock:
            journal = list(self._journal)
        if path_pattern is None:
            return journal
        return [call for call in journal if re.fullmatch(path_pattern, call["path"])]

    def scripts(self):
        with self._lock:
            return list(self._scripts)

    def reset(self):
        with self._lock:
            self._scripts.clear()
            self._journal.clear()


CONTROL = Control()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        path = urlparse(self.path).path
        body = self._read_json()

        if path.startswith("/_control/"):
            self._control_post(path, body)
            return

        # A retry of the same request carries the same Idempotency-Key and must
        # get the same identifiers back; a genuinely new call gets new ones.
        attempt_key = self.headers.get("Idempotency-Key") or f"{path}:{uuid.uuid4()}"
        answer = route(path, body, attempt_key)
        script = CONTROL.claim(path, body) if CONTROL_ENABLED else None

        if script is None and answer is None:
            # Declaring an operation this fixture does not answer should be
            # loud, not a silent empty body the engine rejects downstream.
            print(f"unhandled {path}", flush=True)
            if CONTROL_ENABLED:
                CONTROL.record(path, body, self.headers, 501)
            self.send_error(501, "no mock answer for this operation")
            return

        status = 200
        if script is not None:
            status = script["status"]
            if script["delay_ms"]:
                # A held answer is how a test drives the connector's own
                # timeout and its retry policy.
                time.sleep(script["delay_ms"] / 1000.0)
            if script["body"] is not None:
                answer = script["body"]
            elif script["patch"]:
                # Merged over the default so the echoed identifiers a Process
                # asserts on stay correct while one field is steered.
                answer = {**(answer or {}), **script["patch"]}

        if CONTROL_ENABLED:
            CONTROL.record(path, body, self.headers, status)
        print(f"{path} -> {status}", flush=True)
        self._send_json(status, answer if answer is not None else {})

    def do_GET(self):
        path = urlparse(self.path).path
        if path.startswith("/_control/"):
            self._control_get(path)
            return
        # Compose health probe.
        self._send_json(200, {"status": "ok", "control": CONTROL_ENABLED})

    # -- control plane -------------------------------------------------------

    def _control_post(self, path, body):
        if not CONTROL_ENABLED:
            self.send_error(404, "control plane disabled")
            return
        if path == "/_control/reset":
            CONTROL.reset()
            self._send_json(200, {"reset": True})
        elif path == "/_control/script":
            self._send_json(200, {"scripted": CONTROL.add_script(body)})
        else:
            self._send_json(404, {"error": "unknown control endpoint", "help": CONTROL_HELP})

    def _control_get(self, path):
        if not CONTROL_ENABLED:
            self.send_error(404, "control plane disabled")
            return
        if path == "/_control/calls":
            query = parse_qs(urlparse(self.path).query)
            pattern = query.get("path", [None])[0]
            self._send_json(200, {"calls": CONTROL.calls(pattern)})
        elif path == "/_control/scripts":
            self._send_json(200, {"scripts": CONTROL.scripts()})
        elif path == "/_control/help":
            self._send_json(200, CONTROL_HELP)
        else:
            self._send_json(404, {"error": "unknown control endpoint", "help": CONTROL_HELP})

    # -- plumbing ------------------------------------------------------------

    def _read_json(self):
        length = int(self.headers.get("content-length", 0))
        raw = self.rfile.read(length) if length else b"{}"
        try:
            parsed = json.loads(raw)
        except json.JSONDecodeError:
            return {}
        return parsed if isinstance(parsed, dict) else {}

    def _send_json(self, status, payload):
        encoded = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def log_message(self, *_args):
        pass


if __name__ == "__main__":
    control = " (+/_control)" if CONTROL_ENABLED else ""
    print(f"mock providers listening on {PORT}{control}", flush=True)
    # Threaded: a system test scripts an answer or reads the journal while the
    # engine is mid-call, and one racing scenario has two calls in flight.
    ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
