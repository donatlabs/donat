"""Stand-in payment, tax, carrier, payout and notification providers.

The Petshop connectors are declared against five external HTTP services. This
answers them so the example runs end to end without an account anywhere. It is
a fixture, not a simulator: every answer is the success shape its operation
declares, and identifiers the caller sent are echoed back, because a Process
asserts that an answer belongs to the request it was given.

The engine treats an answer as untrusted input and validates it against the
operation's declared response contract, so an answer that drifts from the
declaration fails the activity instead of propagating.
"""

import json
import re
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = 8099

EVIDENCE = {"provider": "mock", "example": "petshop"}


def route(path, body):
    """The success answer for one operation, or None if nothing declares it."""

    # -- tax -----------------------------------------------------------------
    if path == "/v1/tax-quotes":
        return {
            "tax_quote_id": "tax_quote_1",
            "checkout_quote_id": body.get("checkout_quote_id"),
            "tax_minor": 160,
            "tax_code": "US-STANDARD",
            "currency": body.get("currency", "USD"),
        }

    # -- payment -------------------------------------------------------------
    if path == "/v1/payment-authorizations":
        return {
            "provider_event_id": "evt_authorize_1",
            "authorization_id": "auth_1",
            "provider_reference": "ref_1",
            "status": "authorized",
            "normalized_payload": EVIDENCE,
        }
    if re.fullmatch(r"/v1/payment-authorizations/[^/]+/captures", path):
        return {
            "payment_id": body.get("payment_id"),
            "shipment_id": body.get("shipment_id"),
            "amount_minor": body.get("amount_minor"),
            "provider_event_id": "evt_capture_1",
            "capture_id": "cap_1",
            "provider_reference": "ref_1",
            "status": "captured",
            "normalized_payload": EVIDENCE,
        }
    if re.fullmatch(r"/v1/payment-authorizations/[^/]+/voids", path):
        return {
            "provider_event_id": "evt_void_1",
            "void_id": "void_1",
            "provider_reference": "ref_1",
            "status": "voided",
            "normalized_payload": EVIDENCE,
        }
    if re.fullmatch(r"/v1/payment-authorizations/[^/]+/refunds", path):
        return {
            "provider_event_id": "evt_refund_1",
            "refund_id": "refund_1",
            "provider_reference": "ref_1",
            "status": "refunded",
            "normalized_payload": EVIDENCE,
        }
    if path == "/v1/payment-reconciliations":
        # Agreeing with the store on every compared fact is what makes a
        # reconciliation match automatically instead of waiting for support.
        return {
            "provider_event_id": body.get("provider_event_id"),
            "reconciliation_id": "recon_1",
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
            "provider_event_id": "evt_lookup_absent",
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
            "carrier_shipment_reference": "carrier_ref_1",
            "tracking_number": "TRACK-1",
            "label_url": "https://carrier.example/labels/1.pdf",
            "event_id": "evt_label_1",
            "outcome": "label_created",
        }
    if re.fullmatch(r"/v1/returns/[^/]+/labels", path):
        return {
            "provider_event_id": "evt_return_label_1",
            "return_id": body.get("return_key"),
            "tracking_number": "RETURN-TRACK-1",
            "label_url": "https://carrier.example/returns/1.pdf",
            "status": "created",
            "normalized_payload": EVIDENCE,
        }

    # -- payout --------------------------------------------------------------
    if path == "/v1/payouts":
        return {
            "local_payout_id": body.get("payout_id"),
            "vendor_id": body.get("vendor_id"),
            "provider_event_id": "evt_payout_1",
            "payout_id": "payout_1",
            "outcome": "paid",
            "normalized_payload": EVIDENCE,
        }

    return None


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        raw = self.rfile.read(length) if length else b"{}"
        try:
            body = json.loads(raw)
        except json.JSONDecodeError:
            body = {}

        answer = route(self.path, body)
        if answer is None:
            # Declaring an operation this fixture does not answer should be
            # loud, not a silent empty body the engine rejects downstream.
            print(f"unhandled {self.path}", flush=True)
            self.send_error(501, "no mock answer for this operation")
            return

        print(f"{self.path} -> 200", flush=True)
        payload = json.dumps(answer).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        # Compose health probe.
        self.send_response(200)
        self.send_header("content-length", "2")
        self.end_headers()
        self.wfile.write(b"ok")

    def log_message(self, *_args):
        pass


if __name__ == "__main__":
    print(f"mock providers listening on {PORT}", flush=True)
    HTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
