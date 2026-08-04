"""The shopper's journey, expressed the way a tester would walk it.

Everything here goes through the public API under an explicit role — no
database, no seeding shortcut. If a step cannot be done through the API, that
is a finding about the store, not something to work around here.
"""

from __future__ import annotations

import uuid
from typing import Any

from .client import Actor
from .wait import until

# Seeded by the store's own migrations. The suite reads them rather than
# assuming: a changed fixture should fail a catalogue test, not every test.
CUSTOMER_ONE = "customer-1"
CUSTOMER_TWO = "customer-2"

#: A variant that is active, published and in stock in the seeded catalogue.
IN_STOCK_VARIANT = 2

#: An order that reached a terminal outcome of the checkout Process.
CHECKOUT_TERMINAL_STATUSES = {"authorized", "paid", "cancelled", "payment_failed", "declined"}


def new_request_id() -> str:
    """A fresh idempotency key. Reuse one on purpose to test replay."""

    return str(uuid.uuid4())


# -- catalogue --------------------------------------------------------------

PUBLISHED_CATALOGUE = """
query Catalogue {
  product(order_by: {id: asc}) {
    id
    slug
    title
    status
    variants(order_by: {id: asc}) { id sku price_minor currency active }
  }
}
"""


def catalogue(actor: Actor) -> list[dict]:
    return actor.query(PUBLISHED_CATALOGUE)["product"]


def ensure_stock(staff: Actor, variant_id: int, at_least: int = 200) -> None:
    """Top the shelf up, the way a store employee would.

    A suite run against a long-lived stand consumes real inventory: without
    this, the tenth run fails on an empty shelf rather than on a defect.
    """

    current = stock(staff, variant_id)
    assert current is not None, f"variant {variant_id} has no stock row"
    if current["on_hand"] >= at_least:
        return
    staff.graphql(
        """
        mutation Restock($variant: Int!, $on_hand: Int!) {
          update_inventory_stock(
            where: {variant_id: {_eq: $variant}}, _set: {on_hand: $on_hand}
          ) { affected_rows }
        }
        """,
        {"variant": variant_id, "on_hand": at_least},
    ).unwrap()


def stock(actor: Actor, variant_id: int) -> dict | None:
    rows = actor.query(
        """
        query Stock($variant: Int!) {
          inventory_stock(where: {variant_id: {_eq: $variant}}) {
            variant_id
            on_hand
            reserved
          }
        }
        """,
        {"variant": variant_id},
    )["inventory_stock"]
    return rows[0] if rows else None


# -- cart -------------------------------------------------------------------


def current_open_cart(customer: Actor) -> dict | None:
    """The caller's open cart, if they have one.

    The store allows exactly one (`cart_one_open_per_customer`), which is what
    a shopper experiences: a basket, not a pile of baskets.
    """

    rows = customer.query(
        """
        query OpenCart {
          cart(where: {status: {_eq: cart_open}}, order_by: {id: desc}) {
            id
            status
            lines { id }
          }
        }
        """
    )["cart"]
    return rows[0] if rows else None


def open_cart(customer: Actor) -> int:
    """An empty open cart owned by the calling customer.

    Reuses the one open cart the store permits, emptying it first, so a suite
    run against a long-lived stand starts from the same place every time.
    """

    existing = current_open_cart(customer)
    if existing is not None:
        for line in existing["lines"]:
            customer.graphql(
                "mutation DropLine($id: bigint!) { delete_cart_line(where: {id: {_eq: $id}}) { affected_rows } }",
                {"id": line["id"]},
            ).unwrap()
        return existing["id"]
    data = customer.query("mutation OpenCart { insert_cart_one(object: {}) { id status } }")
    return data["insert_cart_one"]["id"]


def add_line(customer: Actor, cart_id: int, variant_id: int, quantity: int = 1):
    """Put a variant in the cart. Returns the raw answer: refusals are cases."""

    return customer.graphql(
        """
        mutation AddLine($cart: bigint!, $variant: Int!, $quantity: Int!) {
          insert_cart_line_one(
            object: {cart_id: $cart, variant_id: $variant, quantity: $quantity}
          ) { id cart_id variant_id quantity }
        }
        """,
        {"cart": cart_id, "variant": variant_id, "quantity": quantity},
    )


def cart_with_one_line(customer: Actor, variant_id: int = IN_STOCK_VARIANT, quantity: int = 1) -> int:
    cart_id = open_cart(customer)
    add_line(customer, cart_id, variant_id, quantity).unwrap()
    return cart_id


def read_cart(customer: Actor, cart_id: int) -> dict | None:
    rows = customer.query(
        """
        query Cart($cart: bigint!) {
          cart(where: {id: {_eq: $cart}}) {
            id
            status
            lines(order_by: {id: asc}) { id variant_id quantity }
          }
        }
        """,
        {"cart": cart_id},
    )["cart"]
    return rows[0] if rows else None


# -- checkout ---------------------------------------------------------------


def start_checkout(customer: Actor, cart_id: int, request_id: str | None = None):
    """The entry-point Command. The durable Process does the rest."""

    return customer.graphql(
        """
        mutation StartCheckout($cart: bigint!, $request: uuid!) {
          start_checkout(cart_id: $cart, request_id: $request) { cart_id owner_user_id }
        }
        """,
        {"cart": cart_id, "request": request_id or new_request_id()},
    )


def orders_of(customer: Actor) -> list[dict]:
    return customer.query(
        """
        query MyOrders {
          orders(order_by: {created_at: desc}) {
            id
            order_status
            total_minor
            currency
            created_at
          }
        }
        """
    )["orders"]


def await_new_order(customer: Actor, *, known: set[str], timeout: float) -> dict:
    """The order the checkout Process created, once it exists."""

    def unseen() -> list[dict]:
        return [order for order in orders_of(customer) if order["id"] not in known]

    return until(
        unseen,
        lambda orders: bool(orders),
        timeout=timeout,
        description="the checkout Process to create an order",
    )[0]


def await_order_status(customer: Actor, order_id: str, expected: set[str], *, timeout: float) -> dict:
    """Poll one order until it reaches one of the expected statuses."""

    def read() -> dict | None:
        rows = customer.query(
            """
            query Order($id: uuid!) {
              orders(where: {id: {_eq: $id}}) {
                id
                order_status
                total_minor
                currency
              }
            }
            """,
            {"id": order_id},
        )["orders"]
        return rows[0] if rows else None

    return until(
        read,
        lambda order: order is not None and order["order_status"] in expected,
        timeout=timeout,
        description=f"order {order_id} to reach {sorted(expected)}",
    )


def checkout_to_order(customer: Actor, cart_id: int, *, timeout: float, request_id: str | None = None) -> dict:
    """Start checkout and wait for the order the Process commits."""

    before = {order["id"] for order in orders_of(customer)}
    start_checkout(customer, cart_id, request_id).unwrap()
    return await_new_order(customer, known=before, timeout=timeout)


# -- money ------------------------------------------------------------------


def payments_of(actor: Actor, order_id: str) -> list[dict]:
    """Payments as support sees them: the store's own account of the money."""

    return actor.query(
        """
        query Payments($order: uuid!) {
          payment(where: {order_id: {_eq: $order}}, order_by: {created_at: asc}) {
            id
            order_id
            status
            amount_minor
            currency
          }
        }
        """,
        {"order": order_id},
    )["payment"]


def await_payment_status(actor: Actor, order_id: str, expected: set[str], *, timeout: float) -> dict:
    def read() -> dict | None:
        payments = payments_of(actor, order_id)
        return payments[-1] if payments else None

    return until(
        read,
        lambda payment: payment is not None and payment["status"] in expected,
        timeout=timeout,
        description=f"payment for order {order_id} to reach {sorted(expected)}",
    )


def total_of(order: dict) -> int:
    return int(order["total_minor"])


def json_path(value: Any, path: str, default: Any = None) -> Any:
    for segment in path.strip("/").split("/"):
        if isinstance(value, dict) and segment in value:
            value = value[segment]
        elif isinstance(value, list) and segment.isdigit() and int(segment) < len(value):
            value = value[int(segment)]
        else:
            return default
    return value
