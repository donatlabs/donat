"""A client for the lending service, and nothing that knows how it works.

The suite drives the library the way a member does: GraphQL over HTTP, as one
explicit role. It never imports the engine and never reads the database to
decide whether a test passed, because the question it asks is whether the
deployed thing behaves like a library — not whether the code does what the
code says.
"""

from __future__ import annotations

import dataclasses
import datetime as dt
import os
import time
import uuid
from typing import Any

import requests

MEMBER = "member"
LIBRARIAN = "librarian"


@dataclasses.dataclass(frozen=True)
class Stand:
    """One running lending service.

    `name` is what the parameterised tests are labelled with, so a failure
    report says which host disagreed.
    """

    name: str
    base_url: str

    @staticmethod
    def from_env() -> list["Stand"]:
        stands = []
        engine = os.environ.get("LENDING_ENGINE_URL")
        if engine:
            stands.append(Stand("engine", engine.rstrip("/")))
        host = os.environ.get("LENDING_GO_URL")
        if host:
            stands.append(Stand("go-host", host.rstrip("/")))
        return stands


class GraphQLError(Exception):
    """A rejection the service returned, with its structured error intact."""

    def __init__(self, errors: list[dict[str, Any]]):
        self.errors = errors
        super().__init__(self.message)

    @property
    def message(self) -> str:
        if not self.errors:
            return ""
        return str(self.errors[0].get("message", ""))

    @property
    def code(self) -> str:
        if not self.errors:
            return ""
        return str(self.errors[0].get("extensions", {}).get("code", ""))


class Library:
    """The service under test."""

    def __init__(self, stand: Stand, timeout: float = 15.0):
        self.stand = stand
        self.timeout = timeout
        self.session = requests.Session()
        self.member_id: str | None = None

    # -- transport ---------------------------------------------------------

    def is_up(self) -> bool:
        try:
            r = self.session.get(f"{self.stand.base_url}/healthz", timeout=self.timeout)
            return r.status_code < 500
        except requests.RequestException:
            return False

    def call(
        self,
        role: str,
        query: str,
        variables: dict[str, Any] | None = None,
        *,
        member_id: str | None = None,
    ) -> dict[str, Any]:
        """Run one operation as `role`, raising GraphQLError on a rejection."""
        headers = {"Content-Type": "application/json"}
        if role:
            headers["X-Donat-Role"] = role
        identity = member_id if member_id is not None else self.member_id
        if role == MEMBER and identity:
            headers["X-Donat-User-Id"] = identity
        secret = os.environ.get("LENDING_ADMIN_SECRET")
        if secret:
            # The standalone engine only honours X-Donat-* headers on a trusted
            # request. The Go host resolves them directly and ignores this.
            headers["X-Donat-Admin-Secret"] = secret

        payload: dict[str, Any] = {"query": query}
        if variables:
            payload["variables"] = variables
        r = self.session.post(
            f"{self.stand.base_url}/v1/graphql",
            json=payload,
            headers=headers,
            timeout=self.timeout,
        )
        body = r.json()
        if body.get("errors"):
            raise GraphQLError(body["errors"])
        return body.get("data") or {}

    def attempt(self, *args, **kwargs) -> GraphQLError | None:
        """Run an operation expected to be refused; return the rejection."""
        try:
            self.call(*args, **kwargs)
        except GraphQLError as e:
            return e
        return None

    # -- fixtures the librarian sets up ------------------------------------

    def add_member(self, name: str, loan_limit: int) -> str:
        data = self.call(
            LIBRARIAN,
            """mutation ($name: String!, $limit: Int!) {
                 insert_member(objects: [{ name: $name, loan_limit: $limit }]) {
                   returning { id }
                 }
               }""",
            {"name": name, "limit": loan_limit},
        )
        member_id = data["insert_member"]["returning"][0]["id"]
        self.member_id = member_id
        return member_id

    def add_book(self, title: str, author: str) -> str:
        data = self.call(
            LIBRARIAN,
            """mutation ($title: String!, $author: String!) {
                 insert_book(objects: [{ title: $title, author: $author }]) {
                   returning { id }
                 }
               }""",
            {"title": title, "author": author},
        )
        return data["insert_book"]["returning"][0]["id"]

    def add_copy(self, book_id: str, label: str | None = None) -> str:
        data = self.call(
            LIBRARIAN,
            """mutation ($book: uuid!, $label: String!) {
                 insert_copy(objects: [{ book_id: $book, label: $label, status: "available" }]) {
                   returning { id }
                 }
               }""",
            {"book": book_id, "label": label or f"c-{uuid.uuid4().hex[:8]}"},
        )
        return data["insert_copy"]["returning"][0]["id"]

    # -- the commands ------------------------------------------------------

    def borrow(self, copy_id: str, days: int = 14, **kwargs) -> dict[str, Any]:
        data = self.call(
            MEMBER,
            """mutation ($copy: uuid!, $from: date!, $due: date!, $req: uuid!) {
                 borrow_copy(copy_id: $copy, borrowed_on: $from, due_on: $due, request_id: $req) {
                   loan_id
                   copy_id
                   due_on
                   open_loans_before
                 }
               }""",
            {"copy": copy_id, "from": today(), "due": plus_days(days),
             "req": str(uuid.uuid4())},
            **kwargs,
        )
        return data["borrow_copy"]

    def return_copy(self, loan_id: str, **kwargs) -> dict[str, Any]:
        data = self.call(
            MEMBER,
            """mutation ($loan: uuid!, $on: date!) {
                 return_copy(loan_id: $loan, returned_on: $on) {
                   loan_id
                   copy_id
                   copy_status
                 }
               }""",
            {"loan": loan_id, "on": today()},
            **kwargs,
        )
        return data["return_copy"]

    def extend(self, loan_id: str, days: int, **kwargs) -> dict[str, Any]:
        data = self.call(
            MEMBER,
            """mutation ($loan: uuid!, $due: date!) {
                 extend_loan(loan_id: $loan, new_due_on: $due) {
                   loan_id
                   due_on
                   extensions
                 }
               }""",
            {"loan": loan_id, "due": plus_days(days)},
            **kwargs,
        )
        return data["extend_loan"]

    # -- reads -------------------------------------------------------------

    def copy_status(self, copy_id: str) -> str:
        data = self.call(
            LIBRARIAN,
            """query ($id: uuid!) { copy(where: { id: { _eq: $id } }) { status } }""",
            {"id": copy_id},
        )
        rows = data["copy"]
        return rows[0]["status"] if rows else ""

    def followup_note(self, loan_id: str, timeout: float = 20.0) -> str | None:
        """Wait for the durable follow-up on a loan, or give up.

        The row is written by a Process, not by the borrow that started it, so
        it appears some time after the mutation returned. Polling is what a
        caller would actually do; the timeout is what stops a stand with
        nothing driving its Processes from hanging the suite instead of
        failing it.
        """
        deadline = time.monotonic() + timeout
        while True:
            data = self.call(
                LIBRARIAN,
                """query ($id: uuid!) {
                     loan_followup(where: { loan_id: { _eq: $id } }) { note }
                   }""",
                {"id": loan_id},
            )
            rows = data["loan_followup"]
            if rows:
                return str(rows[0]["note"])
            if time.monotonic() >= deadline:
                return None
            time.sleep(0.25)

    def open_loans(self, member_id: str) -> int:
        data = self.call(
            LIBRARIAN,
            """query ($id: uuid!) {
                 loan(where: { member_id: { _eq: $id }, status: { _eq: "active" } }) { id }
               }""",
            {"id": member_id},
        )
        return len(data["loan"])


def today() -> str:
    return dt.date.today().isoformat()


def plus_days(n: int) -> str:
    return (dt.date.today() + dt.timedelta(days=n)).isoformat()
