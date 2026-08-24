"""Somebody trying to get at the bytes.

A file in this store is a column, and its address is a capability: an upload URL
signed for one object, a completion URL signed for one upload. Whoever holds a
valid one may use it — so what has to hold is that a caller cannot *make* one,
cannot bend one into pointing somewhere else, and cannot discover one that was
never shown to them.

The avatar column is declared `public`, which means its download URL is a grant
rather than a secret. That is the design (ADR 033), so these cases test the wall
that remains: the store decides who can *learn* the address.
"""

from __future__ import annotations

import uuid

import pytest
import requests

from petshop_qa import domain as d

from test_file_attachments import (
    PNG,
    avatar_of,
    claim,
    complete,
    granted_upload,
    put_bytes,
)

pytestmark = pytest.mark.serial


def tampered(signature_query: str) -> str:
    """The same URL with one character of its signature changed."""

    marker = "X-Amz-Signature=" if "X-Amz-Signature=" in signature_query else "sig="
    head, _, tail = signature_query.partition(marker)
    flipped = ("0" if tail[0] != "0" else "1") + tail[1:]
    return f"{head}{marker}{flipped}"


# -- forging the upload capability -------------------------------------------


def test_bytes_cannot_be_stored_without_the_store_having_signed_for_them(shopper):
    """The object store is not writable by whoever guesses a path.

    An upload URL is signed for one object. A signature that was not made by
    this deployment buys nothing — otherwise the bucket would be a public
    drop box with a long name.
    """

    upload = granted_upload(shopper)

    refused = requests.put(
        tampered(upload["url"]), data=PNG, headers={"content-type": "image/png"}, timeout=30
    )

    assert refused.status_code in {400, 401, 403}, (
        f"tampered upload signature was accepted: {refused.status_code}"
    )


def test_an_upload_signature_cannot_be_pointed_at_another_object(shopper):
    """The signature covers the object, not the bucket.

    Moving a valid signature onto a different key is how a caller would
    overwrite somebody else's file with their own bytes.
    """

    upload = granted_upload(shopper)
    address, _, query = upload["url"].partition("?")
    elsewhere = f"{address.rsplit('/', 1)[0]}/{uuid.uuid4()}.part?{query}"

    refused = requests.put(
        elsewhere, data=PNG, headers={"content-type": "image/png"}, timeout=30
    )

    assert refused.status_code in {400, 401, 403}, (
        f"a signature was reused for another object: {refused.status_code}"
    )


def test_a_completion_cannot_be_signed_by_the_caller(shopper, store):
    """Completion is the store's word that the bytes arrived.

    Its URL is signed by the engine. A caller who could forge it could mark an
    upload complete for bytes nobody stored — which is exactly what the claim
    gate exists to prevent.
    """

    upload = granted_upload(shopper)
    assert put_bytes(upload) in {200, 204}

    forged = requests.post(
        f"{store.config.base_url}{tampered(upload['complete_url'])}", timeout=30
    )

    assert forged.status_code in {400, 401, 403, 404}, (
        f"a forged completion was accepted: {forged.status_code}"
    )
    # And the upload is still incomplete, so it cannot be claimed onto a row.
    refused = claim(shopper, upload["id"])
    assert refused.errors, "an unfinished upload was claimed after a forged completion"


@pytest.mark.parametrize(
    "path",
    [
        # Only the raw traversal stays here: a URL library normalizes it away
        # before the request is sent, so exercising the engine with it takes a
        # client that transmits the path verbatim. The payloads that survive
        # normalization (an encoded dot-segment, an unissued uuid) are ported
        # to examples/petshop/metadata/storage_test.yaml.
        "/v1/files/complete/../../v1/graphql",
    ],
)
def test_a_completion_path_is_not_a_way_into_the_engine(store, shopper, path):
    """Whatever is put in the path, it is read as an upload id or refused."""

    answer = requests.post(f"{store.config.base_url}{path}", timeout=30)

    assert answer.status_code in {400, 401, 403, 404, 405}, (
        f"{path} answered {answer.status_code}"
    )
    # The store is still itself afterwards.
    assert d.catalogue(shopper), "the shop stopped answering after a crafted file path"


# -- using somebody else's capability ----------------------------------------


def test_another_session_cannot_finish_an_upload_it_did_not_ask_for(
    shopper, other_shopper, store
):
    """An upload belongs to the session that minted it.

    The completion URL is a capability, so this is about the gate behind it:
    even with the bytes in place, the upload becomes a column value only for
    the session that asked for it.
    """

    upload = granted_upload(shopper)
    assert put_bytes(upload) in {200, 204}
    assert complete(shopper, upload).status_code in {200, 204}

    stolen = claim(other_shopper, upload["id"])

    assert stolen.errors or stolen.value("data/update_customer/affected_rows") == 0, (
        f"another shopper claimed an upload they never asked for: {stolen.text[:200]}"
    )
    # The rightful owner can still use it, so the refusal was about who asked.
    claim(shopper, upload["id"]).unwrap()
    assert avatar_of(shopper)["id"] == upload["id"]


def test_a_published_file_is_a_grant_but_its_address_is_still_earned(
    shopper, other_shopper, support
):
    """A public URL is readable by whoever holds it — and nobody else is told it.

    That is what publishing means, so the test is about discovery: the shopper
    next door cannot read the column, and therefore never learns the address,
    while a desk that may see the customer does.
    """

    upload = granted_upload(shopper)
    assert put_bytes(upload) in {200, 204}
    assert complete(shopper, upload).status_code in {200, 204}
    claim(shopper, upload["id"]).unwrap()
    published = avatar_of(shopper)
    assert published and published["url"], "the shopper's own avatar has an address"

    # The other shopper reads the customer table and finds only themselves.
    theirs = other_shopper.query(
        "query { customer { customer_id avatar { id url } } }"
    )["customer"]
    assert all(row["customer_id"] != d.CUSTOMER_ONE for row in theirs), (
        f"another shopper read the avatar owner's row: {theirs}"
    )

    # Held, the URL works — which is the grant, stated rather than assumed.
    fetched = requests.get(published["url"], timeout=30)
    assert fetched.status_code == 200 and fetched.content == PNG
