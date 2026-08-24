"""A shopper puts a picture on their profile.

No byte of a file passes through the engine: it hands out a presigned URL, the
client uploads straight to object storage, tells the engine it finished, and
then claims the id into the column that declares the attachment. This walks
that whole path the way a browser would, including the parts that must be
refused.
"""

from __future__ import annotations

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

    A session holds at most ten unclaimed uploads and earlier runs may have
    spent them; the collector reclaims abandoned ones a day later, so on a
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


# -- what the store refuses --------------------------------------------------


def test_a_shopper_cannot_claim_an_upload_somebody_else_asked_for(shopper, other_shopper):
    """An upload is bound to the session that asked for it."""

    upload = granted_upload(shopper)
    assert put_bytes(upload) in {200, 204}
    complete(shopper, upload)

    stolen = claim(other_shopper, upload["id"])

    assert stolen.errors, "another shopper cannot wear this picture"
    assert stolen.error_code() in {"validation-failed", "permission-error"}
