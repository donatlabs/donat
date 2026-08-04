"""A shopper puts a picture on their profile.

No byte of a file passes through the engine: it hands out a presigned URL, the
client uploads straight to object storage, tells the engine it finished, and
then claims the id into the column that declares the attachment. This walks
that whole path the way a browser would, including the parts that must be
refused.
"""

from __future__ import annotations

import uuid

import pytest
import requests

from petshop_qa import domain as d

# One-pixel PNG. Small enough to post, real enough to satisfy a media type.
PNG = bytes.fromhex(
    "89504e470d0a1a0a0000000d494844520000000100000001080600000"
    "01f15c4890000000a49444154789c6360000002000155a2b5ee000000"
    "0049454e44ae426082"
)
ATTACHMENT = "public_customer_avatar"


def request_upload(actor, *, file_name="avatar.png", media_type="image/png", size=len(PNG)):
    return actor.graphql(
        """
        mutation Upload(
          $attachment: donat_file_attachment!, $file_name: String!,
          $media_type: String!, $size: Int!
        ) {
          donat_request_file_upload(
            attachment: $attachment, file_name: $file_name,
            media_type: $media_type, size: $size
          ) { id url method complete_url expires_at }
        }
        """,
        {
            "attachment": ATTACHMENT,
            "file_name": file_name,
            "media_type": media_type,
            "size": size,
        },
    )


def granted_upload(actor, **kwargs) -> dict:
    """An upload URL, or a skip that says how to get the allowance back.

    A session holds at most ten unclaimed uploads and the refusal cases below
    spend them; the collector reclaims abandoned ones a day later, so on a
    reused stand `stack.sh provision` is what clears them.
    """

    answer = request_upload(actor, **kwargs)
    if answer.errors and "unclaimed uploads" in (answer.error_message() or ""):
        pytest.skip(
            "this stand's upload allowance is spent by earlier runs; "
            "run tests-system/stack.sh provision (or raise a fresh stand)"
        )
    return answer.unwrap()["donat_request_file_upload"]


def put_bytes(upload: dict, payload: bytes = PNG, media_type: str = "image/png") -> int:
    """Upload straight to object storage, as the browser is told to."""

    response = requests.request(
        upload["method"],
        upload["url"],
        data=payload,
        headers={"content-type": media_type},
        timeout=30,
    )
    return response.status_code


def complete(actor, upload: dict) -> requests.Response:
    """Tell the engine the bytes are there. The call carries no other proof.

    `complete_url` is a signed path on the store itself, not an absolute URL:
    the signature is the authority, and the host is wherever the client already
    is talking to.
    """

    return requests.post(f"{actor._config.base_url}{upload['complete_url']}", timeout=30)


def claim(actor, file_id: str):
    return actor.graphql(
        """
        mutation Claim($avatar: uuid!) {
          update_customer(where: {}, _set: {avatar: $avatar}) { affected_rows }
        }
        """,
        {"avatar": file_id},
    )


def avatar_of(actor) -> dict | None:
    rows = actor.query(
        "query { customer { customer_id avatar { id file_name media_type size url } } }"
    )["customer"]
    return rows[0]["avatar"] if rows else None


# -- the whole way up and back ----------------------------------------------


def test_a_shopper_uploads_an_avatar_and_reads_it_back(shopper):
    upload = granted_upload(shopper)

    assert upload["url"], "the store hands out somewhere to put the bytes"
    assert upload["complete_url"], "and a way to say the bytes arrived"
    assert put_bytes(upload) in {200, 204}, "object storage accepts the presigned upload"
    assert complete(shopper, upload).status_code in {200, 204}

    claim(shopper, upload["id"]).unwrap()

    stored = avatar_of(shopper)
    assert stored is not None, "the claimed file is on the profile"
    assert stored["id"] == upload["id"]
    assert stored["media_type"] == "image/png"
    assert stored["size"] == len(PNG)

    # The avatar is declared public, so its URL is stable and unsigned — and it
    # must actually serve the bytes that were uploaded.
    fetched = requests.get(stored["url"], timeout=30)
    assert fetched.status_code == 200, f"{stored['url']} -> {fetched.status_code}"
    assert fetched.content == PNG, "what comes back is what went up"


def test_the_engine_never_carries_the_bytes(shopper):
    """The upload URL points at the object store, not at the engine."""

    upload = granted_upload(shopper)

    assert upload["url"].startswith("http"), "the upload target is an absolute URL"
    assert shopper._config.base_url not in upload["url"], (
        f"the bytes would pass through the engine: {upload['url']}"
    )
    # The completion call, which carries no bytes, is the store's own signed
    # path — no host, because the client is already talking to it.
    assert upload["complete_url"].startswith("/v1/files/complete/")
    assert "sig=" in upload["complete_url"], "the completion call proves itself"


# -- what the store refuses --------------------------------------------------


def test_a_media_type_the_column_does_not_declare_is_refused(shopper):
    refused = request_upload(shopper, media_type="application/pdf", file_name="invoice.pdf")

    assert refused.errors, "the column declares image/png and image/jpeg only"
    assert refused.error_code() in {"validation-failed", "permission-error"}


def test_a_file_larger_than_the_column_allows_is_refused(shopper):
    refused = request_upload(shopper, size=8 * 1024 * 1024)

    assert refused.errors, "the column declares a 2 MiB ceiling"
    assert refused.error_code() in {"validation-failed", "permission-error"}


def test_the_public_cannot_ask_for_an_upload(anonymous):
    refused = request_upload(anonymous)

    assert refused.errors, "an upload URL is a grant, not a public resource"


def test_a_shopper_cannot_claim_an_upload_somebody_else_asked_for(shopper, other_shopper):
    """An upload is bound to the session that asked for it."""

    upload = granted_upload(shopper)
    assert put_bytes(upload) in {200, 204}
    complete(shopper, upload)

    stolen = claim(other_shopper, upload["id"])

    assert stolen.errors, "another shopper cannot wear this picture"
    assert stolen.error_code() in {"validation-failed", "permission-error"}


def test_an_upload_nobody_finished_cannot_be_claimed(shopper):
    """The bytes were never delivered, so the id is not claimable yet."""

    upload = granted_upload(shopper)

    unfinished = claim(shopper, upload["id"])

    assert unfinished.errors, "a pending upload is not a file"
    assert unfinished.error_code() in {"validation-failed", "permission-error"}


def test_an_invented_file_id_is_refused(shopper):
    invented = claim(shopper, str(uuid.uuid4()))

    assert invented.errors, "a file id nobody issued claims nothing"


# -- how much one session may ask for ----------------------------------------


def test_a_session_may_not_hoard_unclaimed_uploads(other_shopper):
    """Ten pending uploads is the declared ceiling for one shopper.

    An upload URL costs the store a row and a reserved object key, so a client
    that asks and never finishes is throttled by the store itself — a reverse
    proxy cannot see this, because it is counted against rows the engine owns.

    Deliberately last in the file: it spends the whole allowance, and the
    collector reclaims it a day later (`stack.sh provision` on a reused stand).
    """

    refusal = None
    for _ in range(14):
        answer = request_upload(other_shopper, file_name=f"{uuid.uuid4().hex[:8]}.png")
        if answer.errors:
            refusal = answer
            break

    assert refusal is not None, "the store hands out upload URLs without limit"
    assert refusal.error_code() == "validation-failed"
    assert "maximum number of unclaimed uploads" in (refusal.error_message() or ""), (
        f"unexpected refusal: {refusal.error_message()}"
    )
