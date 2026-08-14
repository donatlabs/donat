"""Does the deployed thing behave like a library?

Each test states a rule the YAML declares and then asks the service to break
it. Nothing here recomputes the answer: the borrowing limit lives in
rules.yaml, the atomic hold in borrow-copy.yaml, and this file only checks that
the service enforces them — on every configured stand.
"""

from __future__ import annotations

import uuid

import concurrent.futures

from lending_qa import LIBRARIAN, MEMBER, Library, plus_days, today


def test_borrowing_lends_the_copy(library: Library, shelf):
    (copy_id,) = shelf(1)

    loan = library.borrow(copy_id)

    assert loan["loan_id"], f"borrow returned no loan: {loan}"
    assert loan["copy_id"] == copy_id
    assert library.copy_status(copy_id) == "on_loan"


def test_a_copy_on_loan_cannot_be_lent_again(library: Library, shelf):
    (copy_id,) = shelf(1)
    library.borrow(copy_id)

    refused = library.attempt(
        MEMBER,
        """mutation ($copy: uuid!, $from: date!, $due: date!, $req: uuid!) {
             borrow_copy(copy_id: $copy, borrowed_on: $from, due_on: $due, request_id: $req) { loan_id }
           }""",
        {"copy": copy_id, "from": today(), "due": plus_days(14), "req": str(uuid.uuid4())},
    )

    assert refused is not None, "a copy already on loan was lent twice"
    assert library.open_loans(library.member_id) == 1


def test_the_loan_limit_refuses_one_too_many(library: Library, shelf):
    # The fixture member's limit is 3.
    copies = shelf(4)
    for copy_id in copies[:3]:
        library.borrow(copy_id)

    refused = library.attempt(
        MEMBER,
        """mutation ($copy: uuid!, $from: date!, $due: date!, $req: uuid!) {
             borrow_copy(copy_id: $copy, borrowed_on: $from, due_on: $due, request_id: $req) { loan_id }
           }""",
        {"copy": copies[3], "from": today(), "due": plus_days(14), "req": str(uuid.uuid4())},
    )

    assert refused is not None, "a fourth loan was allowed past a limit of 3"
    assert "maximum number of loans" in refused.message
    assert library.open_loans(library.member_id) == 3
    # The whole statement rolled back, including the hold that ran before the
    # rule was evaluated.
    assert library.copy_status(copies[3]) == "available"


def test_returning_frees_the_limit_and_the_copy(library: Library, shelf):
    copies = shelf(4)
    loans = [library.borrow(c)["loan_id"] for c in copies[:3]]

    assert library.attempt(
        MEMBER,
        """mutation ($copy: uuid!, $from: date!, $due: date!, $req: uuid!) {
             borrow_copy(copy_id: $copy, borrowed_on: $from, due_on: $due, request_id: $req) { loan_id }
           }""",
        {"copy": copies[3], "from": today(), "due": plus_days(14), "req": str(uuid.uuid4())},
    ), "the limit was not enforced before the return"

    returned = library.return_copy(loans[0])
    assert returned["copy_status"] == "available"
    assert library.copy_status(copies[0]) == "available"

    library.borrow(copies[3])
    assert library.open_loans(library.member_id) == 3


def test_a_closed_loan_cannot_be_returned_twice(library: Library, shelf):
    (copy_id,) = shelf(1)
    loan_id = library.borrow(copy_id)["loan_id"]
    library.return_copy(loan_id)

    refused = library.attempt(
        MEMBER,
        """mutation ($loan: uuid!, $on: date!) {
             return_copy(loan_id: $loan, returned_on: $on) { loan_id }
           }""",
        {"loan": loan_id, "on": today()},
    )

    assert refused is not None, "a closed loan was returned a second time"


def test_extending_moves_the_due_date_and_counts(library: Library, shelf):
    (copy_id,) = shelf(1)
    loan_id = library.borrow(copy_id)["loan_id"]

    first = library.extend(loan_id, 21)
    assert int(first["extensions"]) == 1
    assert first["due_on"] == plus_days(21)

    second = library.extend(loan_id, 28)
    assert int(second["extensions"]) == 2


def test_the_extension_limit_refuses_a_third(library: Library, shelf):
    (copy_id,) = shelf(1)
    loan_id = library.borrow(copy_id)["loan_id"]
    library.extend(loan_id, 21)
    library.extend(loan_id, 28)

    refused = library.attempt(
        MEMBER,
        """mutation ($loan: uuid!, $due: date!) {
             extend_loan(loan_id: $loan, new_due_on: $due) { loan_id }
           }""",
        {"loan": loan_id, "due": plus_days(35)},
    )

    assert refused is not None, "a third extension was allowed past a limit of 2"
    assert "maximum number of times" in refused.message


def test_a_member_cannot_touch_another_members_loan(library: Library, shelf):
    (copy_id,) = shelf(1)
    loan_id = library.borrow(copy_id)["loan_id"]

    # A different member, with their own identity, asking to return a loan that
    # is not theirs. The command permission's filter is what refuses it.
    stranger = library.add_member("Stranger", 3)
    refused = library.attempt(
        MEMBER,
        """mutation ($loan: uuid!, $on: date!) {
             return_copy(loan_id: $loan, returned_on: $on) { loan_id }
           }""",
        {"loan": loan_id, "on": today()},
        member_id=stranger,
    )

    assert refused is not None, "one member returned another member's loan"


def test_a_librarian_cannot_invoke_a_member_command(library: Library, shelf):
    (copy_id,) = shelf(1)

    refused = library.attempt(
        LIBRARIAN,
        """mutation ($copy: uuid!, $from: date!, $due: date!, $req: uuid!) {
             borrow_copy(copy_id: $copy, borrowed_on: $from, due_on: $due, request_id: $req) { loan_id }
           }""",
        {"copy": copy_id, "from": today(), "due": plus_days(14), "req": str(uuid.uuid4())},
    )

    assert refused is not None, "a librarian invoked a member-only command"


def test_a_request_with_no_role_is_denied(library: Library):
    # This engine has no admin role: a request that names no role has no
    # authority at all, whatever else it carries.
    refused = library.attempt("", "{ book { id } }")

    assert refused is not None, "a roleless request was served"
    # The two hosts refuse at different points, and both are right. The Go host
    # resolves the caller from headers it was handed, so it can say the role is
    # missing. The standalone engine honours no header: nothing authenticated
    # the request, so it never gets as far as a role to complain about.
    said = refused.message.lower()
    assert "x-donat-role" in said or "authorization" in said, refused.message


def test_concurrent_borrowers_leave_exactly_one_loan(library: Library, shelf):
    (copy_id,) = shelf(1)

    def borrow():
        return library.attempt(
            MEMBER,
            """mutation ($copy: uuid!, $from: date!, $due: date!, $req: uuid!) {
                 borrow_copy(copy_id: $copy, borrowed_on: $from, due_on: $due, request_id: $req) { loan_id }
               }""",
            {"copy": copy_id, "from": today(), "due": plus_days(14), "req": str(uuid.uuid4())},
        )

    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        outcomes = list(pool.map(lambda _: borrow(), range(4)))

    won = sum(1 for o in outcomes if o is None)
    assert won == 1, f"{won} of 4 concurrent borrowers succeeded, want exactly 1"
    assert library.open_loans(library.member_id) == 1


def test_borrowing_starts_a_durable_process_that_completes(library: Library, shelf):
    """The whole point of the effect: work that outlives the request.

    `borrow_copy` declares a `start_process` effect, so the journal row is
    written by the same statement as the loan — it cannot exist for a borrow
    that rolled back. Carrying that Process forward is a runtime loop, and the
    two stands get there differently: the engine stand drives its own, and the
    Go stand's database has an engine beside it doing nothing else. Both must
    end up with the follow-up row, because the declaration is the same.
    """
    (copy_id,) = shelf(1)

    loan = library.borrow(copy_id)

    note = library.followup_note(loan["loan_id"])
    assert note == "borrowed", (
        "the Process the borrow started never reached its command state; "
        "nothing recorded a follow-up for this loan"
    )
