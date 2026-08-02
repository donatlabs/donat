---
type: decision
status: accepted
date: 2026-08-01
features:
  - "[[declarative-saas]]"
  - "[[008-file-attachments]]"
---

# A file is a column, and its URL is signed by the database

## Context

Attaching a file to a row had no answer at all: an application either kept
bytes in a `bytea` column, which drags them through every query that selects
the row, or it ran its own upload service beside the engine and lost the
per-role permission model at the boundary.

The obvious shape — a `donat.attachments` table keyed by `(table, row_id)` —
looked flexible but needed a second authorization model. Nothing in a
polymorphic row says which of the caller's permissions govern it, so the engine
would have had to invent a rule for who may read a file, beside the rule the
owning table already states.

Two engine invariants pulled against the feature. The response is assembled in
Postgres and never walked row by row in Rust (M4), yet a presigned URL is a
per-row value. And a mutation is database work that performs no external I/O,
yet deleting a row should eventually free the bytes it referenced.

## Decision

A file is an ordinary `uuid` column on the application's own table. The table's
metadata says which columns hold files; nothing else changes about the table.
Many files per entity is an ordinary child table with such a column.

The declaration carries no role list. A role may obtain an upload URL for a
column exactly when the table's `insert_permissions` or `update_permissions`
let it write that column, and may read a download URL exactly when its
`select_permissions` expose it — resolved through inherited roles by the same
planner every other field uses. Command-only permissions do not count: they
exist to let a closed command write a table without opening a CRUD root.

Bytes live in an S3-compatible object store and nowhere else. A local-disk store
existed first and was removed: serving bytes from the engine's own origin put
caller-supplied content next to the GraphQL API, made the engine a file server on
the request path, and needed a second signing scheme, a second download route,
a body-streaming limiter and a traversal guard — all to reimplement what an
object store already does, and none of it reusable for the store that a real
deployment uses anyway.

URLs are signed **in SQL**. The engine derives, once per request, the material
that is constant for a statement — a day-scoped subkey for engine-served URLs,
the `kDate → kRegion → kService → kSigning` chain for S3 — and passes it into
the statement. `donat.file_signature`, `donat.file_url` and
`donat.s3_presigned_url` do the per-row half. A query therefore returns a
signed URL per row without the engine touching the response, and the deployment
secret never reaches SQL: what a statement log could retain stops working the
next day.

An upload becomes a column value through a gate in the same statement as the
write. `pending → claimed` happens once, requires a size the engine actually
observed, and is bound to the role and session that minted it; a gate that
matches nothing raises, which rolls the write back with it. A column can
therefore never point at bytes nobody stored.

Deleting is the collector's job, not the mutation's. A write that drops a
reference leaves the object orphaned; a background pass — every `gc.every_days`,
default one day — deletes objects no row references any more and uploads nobody
claimed, storage first and the catalog row only after. That keeps mutations
pure database work and makes a storage failure a retry rather than a leak.

Attachments are Postgres-only in this milestone, and a declaration on any other
source is refused at load time rather than accepted and never served. A command
may not write a file column either: a command step carries no claim gate, so
allowing it would leave exactly the hole the gate exists to close. That is a
restriction with a clear lift path — a gate compiled into command steps — not a
property of the design.

A column may also be declared **public**, and then the URL stops being a
capability at all: a stable, unsigned, immutable address the bytes keep forever,
rooted at a `public_base_url` the operator names. Publishing is a real grant and
is never inferred — anyone holding the URL reads the file, while the row's
select permission still decides who can discover it. What it buys is
proportionate: a CDN and a browser cache it indefinitely, a subscription never
sees it move, and the read costs a string concatenation instead of an HMAC. On
S3 the base URL is required, because the engine cannot know a bucket is
world-readable and a guessed origin would publish links that 403.

Two budgets are counted inside the minting statement itself: unclaimed uploads a
session may hold, and URLs it may mint per minute. The engine does no
network-level rate limiting anywhere — that belongs to the reverse proxy — but
neither of these is visible to a proxy, because both are counted against rows
the engine owns.

## Alternatives

| Option | Why Not |
| --- | --- |
| A polymorphic `donat.attachments` table | Needs a second authorization model: nothing in `(table, row_id)` says which permission governs the file, and FK integrity is lost. |
| A role list on the attachment declaration | A second place to keep in sync with the permission that already decides who may write the column — and an upload a role cannot write is useless to it, because the claim gate refuses it anyway. |
| Sign the URL in Rust after the query | Rewrites the response row by row, which is the one thing the one-statement invariant forbids. |
| Return only the id and make the client call a second endpoint | Costs a round trip per file and puts an authorization decision on a path that no longer knows which row was read. |
| Pass the deployment secret into the statement | A statement log would then hold a key that never expires. The day-scoped subkey signs the same URLs and stops working the next day. |
| Delete the object inside the mutation | Makes a mutation perform external I/O, so a storage outage becomes a failed write and a rollback cannot undo a deleted object. |
| Trust the size the client declared | An S3 upload the engine never sees could then be claimed without existing. The presigned URL binds `content-length` and `content-type`, and the size is recorded from what the store reports. |
| Leave a completed S3 object where the presigned PUT wrote it | That URL cannot be revoked and stays valid for its whole lifetime, so the bytes a claim certified could be replaced afterwards. Completion copies them to a key the URL cannot reach. |
| Sign a public file's URL too, for uniformity | Costs an HMAC per row, expires for no reason, defeats CDN and browser caching, and makes a subscription fire every time the signature is renewed — all to protect bytes the deployment declared world-readable. |
| Let a command write a file column | A command step carries no claim gate, so the column could point at an upload nobody verified — the one invariant this decision is built on. |
| Rate-limit uploads in a reverse proxy alone | A proxy cannot see how many unclaimed rows a session already holds, and that is the resource a caller can actually exhaust. |
| Keep bytes in the database | Drags them through every query that selects the row and gives up the storage tier's cost and lifecycle. |
| Keep a local-disk store beside S3 | Two signing schemes, two download routes, a traversal guard and a body limiter, to serve caller-supplied bytes from the API's own origin — and none of it exercised by a deployment that uses an object store. |
| Verify our signatures against a hand-written stub | A stub can only prove the signature is canonical by our own reading of the spec. The first run against a real MinIO rejected a URL the stub accepted: `X-Amz-SignedHeaders` must percent-encode its separator, because Go's URL parser rejects a query containing a raw `;`. |

## Consequences

An attachment costs one column and two metadata lines, and inherits the row's
permissions exactly. A deployment that declares none is byte-for-byte
unaffected: no route, no root field, no catalog access, no background task, and
no change to any generated schema.

The S3 backend needs one extra call from the client — the presigned `PUT` goes
straight to the store, so the engine only learns the object exists when the
client says so and it asks the store. That is the price of not trusting a
promise: the alternative is a claim that can succeed for an object that was
never uploaded.

Signing now lives in two places that must agree — Rust and SQL. They are pinned
from both ends: a unit test anchors the Rust signer to the published AWS
example, and the conformance suite uploads and downloads through a stub that
recomputes every signature and refuses a wrong one.

A download URL is a bearer capability for its lifetime, exactly like the S3
presigned URL it stands in for. Whoever holds it can read the file until it
expires, regardless of the permission that produced it.

Attachments are also the first feature whose state lives in two places, so a
`pg_dump` alone stops being a complete backup: an object without its row is a
leak the collector cannot see, and a row without its object is a broken link.
The store must be backed up with the database and restored from a point at or
after it.

Because a private URL expires, it cannot be perfectly stable. A stored file never
changes, so the engine signs as of `now` floored to half the lifetime, making a
signature that would otherwise expire the only reason two reads of the same row
differ. A subscription on an unchanged row therefore pushes at most once per
`download_ttl_seconds / 2`. Removing even that would mean dropping the signature
from the payload and authorizing each download against the session instead —
which is the alternative this decision rejected.
