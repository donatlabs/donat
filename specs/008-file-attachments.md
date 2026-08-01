# Spec 008 — File attachments (S3-compatible object storage)

Status: implemented (2026-08-01). Scope agreed with the user:

- a file is attached to an entity through an **ordinary column** on the
  application's own table; the metadata declares that the column holds a file
  reference. Many files per entity is an ordinary child table with such a
  column, not a second attachment model.
- upload uses the **signed-URL** pattern: the client asks the engine for a URL,
  uploads the bytes directly to storage, then submits the returned id in a
  normal `insert`/`update`.
- one store: **S3**, or anything speaking it (MinIO, R2, Ceph). No file byte
  passes through the engine.
- a **background collector** reclaims storage: unclaimed uploads past their TTL
  and objects no row references any more. Every window defaults to one day.

Compatible with the project's posture: `storage.yaml` is deploy-time metadata,
the catalog table is created by `migrate`, there is no admin surface, and every
byte a caller can reach is reachable only through an explicit per-role
permission on the owning row.

## 1. Model

The declaration is split the way the metadata format already splits everything
else. `storage.yaml` holds the deployment-wide half — where bytes go, how URLs
are signed, how often the collector runs — and is written once:

There is deliberately **no local-disk store**. Serving bytes from the engine's
own origin put caller-supplied content next to the GraphQL API, made the engine
a file server on the request path, and needed a second signing scheme, a second
download route and a traversal guard — all to reimplement what an object store
already does. Every attachment goes to S3 or an S3-compatible store (MinIO,
R2, Ceph), and no file byte ever passes through the engine.

```yaml
# metadata/storage.yaml
backends:
  - name: media
    kind: s3
    bucket: donat-media
    region: eu-central-1
    endpoint: https://s3.eu-central-1.amazonaws.com   # optional
    path_style: false                                 # true for MinIO-style hosts
    access_key_id: { value_from_env: DONAT_S3_KEY }
    secret_access_key: { value_from_env: DONAT_S3_SECRET }
    public_base_url: https://cdn.example.com          # required to publish

signing:
  # The store presigns upload and download URLs; the call reporting an upload
  # finished is answered by the engine and carries no other proof.
  secret: { value_from_env: DONAT_FILE_SIGNING_SECRET }
  upload_ttl_seconds: 900
  download_ttl_seconds: 300

gc:
  every_days: 1
  pending_ttl_days: 1
  orphan_grace_days: 1

# What one session may ask for. The engine does no network-level rate limiting
# anywhere — that belongs to the reverse proxy — but these two are counted
# against rows it owns, which a proxy cannot see.
limits:
  pending_uploads_per_session: 20
  uploads_per_minute_per_session: 60

# Which session variable identifies the uploader. An upload is bound to the
# session that asked for it, and this says what "the session" means.
identity:
  session_variable: x-donat-user-id

# Browser origins allowed to upload directly. Empty mounts no CORS at all.
cors:
  allow_origins: ["https://app.example.com"]
```

Which column holds a file is a property of the table, so it is declared in the
table's own file, beside the permissions that govern it:

```yaml
# metadata/databases/default/tables/public_pet.yaml
table: { schema: public, name: pet }
attachments:
  - column: photo
    backend: media
    max_bytes: 5242880
    media_types: [image/png, image/jpeg]
insert_permissions:
  - role: customer
    permission:
      columns: [name, photo, owner_id]
      check: { owner_id: { _eq: X-Donat-User-Id } }
select_permissions:
  - role: customer
    permission:
      columns: "*"
      filter: { owner_id: { _eq: X-Donat-User-Id } }
```

Rules, all checked when metadata is loaded (the engine refuses to serve
otherwise):

- `backend` names a backend declared in `storage.yaml`; `column` exists on the
  table and is `uuid`, nullable or not.
- an attachment is identified by `<schema>.<table>.<column>`; a column is
  declared at most once.
- the declaration carries **no role list**. A role may obtain an upload URL for
  the column exactly when the table's `insert_permissions` or
  `update_permissions` let it write that column, resolved through inherited
  roles by the same planner every other write uses; it may read a download URL
  exactly when its `select_permissions` already expose the column. Anything
  else would be a second authorization model beside the one the table already
  states — and an upload a role cannot write is useless to it, because the
  claim gate (§5) would refuse it anyway. Command-only permissions
  (`command_insert_permissions` and friends) never grant an upload URL: they
  exist to let a closed command write a table without opening a CRUD root.
- `media_types` is an exact allow-list of media types (no wildcards); an empty
  or absent list accepts any type. `max_bytes` is required.
- `signing.secret` is required whenever any table declares an attachment: the
  store presigns the upload and the download, but the completion call is the
  engine's own and carries no other proof.
- attachments are Postgres-only in this milestone: signing, presigning, and the
  claim gate are all compiled as Postgres SQL. A declaration on a SQLite,
  MySQL, or ClickHouse source is refused at load time rather than silently
  ignored.
- `gc.every_days` must be positive.
- **every attachment belongs to the same source.** `donat.file_uploads` lives in
  that source's database, and the file routes and the collector each hold one
  connection to it; rows written to one source and looked up in another would
  simply not be found. Spreading them is refused at load time, the same answer
  the connector registry gives an ambiguous source binding.
- `identity.session_variable` must be a session variable name.
- **a command may not write a declared file column.** A command step never
  passes the claim gate (§5), so it could point a column at an upload nobody
  verified; the declaration is refused at load time, naming the command and the
  column. Filling a file column is the ordinary insert/update path's job. This
  is a restriction, not a law of the design: a future gate compiled into command
  steps would lift it.

### Public files

An attachment may declare `public: true`. Then the bytes are world-readable and
the column returns a **stable, unsigned, immutable URL** instead of a signed one:

```yaml
attachments:
  - column: photo
    backend: media
    max_bytes: 5242880
    public: true
```

Publishing is a real grant and is never inferred. Anyone holding the URL reads
the file regardless of the row's select permission — which still governs who can
*discover* it. In exchange, three things get much better: the URL never changes,
so a CDN and a browser cache it forever (`Cache-Control: …, immutable`); a
subscription on the row never sees it move; and the read costs no HMAC at all,
just a string concatenation in SQL.

Where that URL is rooted is `public_base_url` on the backend — a CDN
distribution, or the bucket's own public origin. On S3 it is **required** before
an attachment may be published: the engine cannot know a bucket is
world-readable, and inventing an origin would publish links that 403. Point it
at a CDN distribution, or at the bucket's own anonymous origin.

Both halves are optional and only their combination does anything. A table
declaring `attachments` with no backends in `storage.yaml` is a load error; a
`storage.yaml` no table refers to is inert. When no table declares an
attachment nothing below exists: no catalog access, no routes, no root field,
no background task, and no change to any generated schema.

## 2. Catalog (`migrations/V…__donat_files.sql`)

```sql
CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE donat.file_uploads (
  id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  attachment     text        NOT NULL,          -- public.pet.photo
  backend        text        NOT NULL,
  object_key     text        NOT NULL,
  file_name      text        NOT NULL,
  media_type     text        NOT NULL,
  declared_bytes bigint      NOT NULL,
  byte_size      bigint,                        -- verified size, set at claim
  state          text        NOT NULL DEFAULT 'pending',   -- pending | claimed
  role           text        NOT NULL,
  session_key    text,
  created_at     timestamptz NOT NULL DEFAULT now(),
  expires_at     timestamptz NOT NULL,
  claimed_at     timestamptz,
  UNIQUE (backend, object_key)
);
```

`state` never returns to `pending`, and a claimed row is never re-claimed, so
one upload backs exactly one column value. Deleting the owning row leaves the
upload orphaned rather than deleting anything: reclaiming bytes is the
collector's job (§6), because a mutation is database work and performs no
external I/O.

## 3. Signing happens in Postgres

Both URLs the caller receives are signed, and both signatures are computed by
SQL inside the same statement that produces them. The engine never rewrites a
response row by row (the M4 one-statement invariant).

The engine derives a date-scoped subkey once per UTC day, in Rust:

```text
k_day = HMAC-SHA256("donat-file-v1" || secret, "YYYYMMDD")
```

and passes `k_day` into the statement as a single `bytea` literal. The secret
itself never reaches SQL, and the material in a statement expires with the day.
The migration installs the per-row half:

```sql
donat.file_signature(k_day bytea, payload text) -> text     -- base64url HMAC
donat.file_token(k_day bytea, purpose text, id uuid, expires bigint) -> text
donat.file_url(k_day bytea, purpose text, path_prefix text,
               id uuid, expires bigint) -> text
donat.s3_presigned_url(k_signing bytea, credential_encoded text, scope text,
                       amz_date text, expires int, origin text, host text,
                       canonical_uri text, method text) -> text
```

`donat.s3_presigned_url` builds an AWS SigV4 canonical request for the row's
object key and finishes the signature with the passed signing key; the
`kDate → kRegion → kService → kSigning` chain is derived in Rust and cached per
day, exactly like the day-scoped subkey behind the completion capability.

Only the URL that is genuinely per-row goes through SQL. An upload URL is minted
once per request, so it is finished in Rust and reaches the statement as a
literal — which is also what lets it bind headers (§4) the SQL half does not
model. The engine-served `donat.file_url` helpers the local-disk store needed
are gone with it; `donat.s3_presigned_url` is the only SQL function left.

## 4. Requesting an upload URL

A single root mutation field exists when at least one attachment is declared,
and its `attachment` enum lists only the columns the requesting role may write
(§1) — so a role that cannot write `pet.photo` cannot even name it:

```graphql
mutation {
  donat_request_file_upload(
    attachment: public_pet_photo
    file_name: "cat.png"
    media_type: "image/png"
    size: 20481
  ) { id url method headers { name value } complete_url expires_at }
}
```

One statement inserts the `pending` row and returns the signed URL:

- a SigV4 presigned `PUT` straight at the bucket, signed over
  `content-length` and `content-type` as well as `host`, so the store itself
  refuses a body of another size or type. It writes to a **staging key**
  (`<attachment>/<id>.part`), never to the address the claimed file will live at.
  `complete_url` is `POST /v1/files/complete/{id}?exp=…&sig=…`: the bytes never
  passed through the engine, so it asks the store what it actually holds, then
  copies the verified object to its final key and drops the staging one. A
  presigned URL cannot be revoked and stays valid for its whole lifetime, so
  anything written to it afterwards is an orphan nothing references.

The client sends every returned header with the upload. Both backends return the
same envelope, so one client works against either. `object_key` is
`<attachment>/<id>` and is engine-chosen; a caller cannot influence the path.

The insert is conditional on the session's own budget — unclaimed uploads it
holds, and URLs it minted in the last minute — counted inside the same statement.
Parallel requests therefore cannot each see room that only one of them has, and
a spent budget is a refusal rather than an empty result.

Refusals use the established envelope: `permission-error` when the role may not
write the column, `validation-failed` when `size` exceeds `max_bytes`,
`media_type` is outside the allow-list, `file_name`/`media_type` are longer than
255 characters, or the session's budget is spent.

## 5. Claiming

The client uploads the bytes, then writes the id like any other column value:

```graphql
mutation { insert_pet_one(object: {name: "Kit", photo: "<id>"}) { id } }
```

The generated statement gains a CTE gate ahead of the write, in the same
statement and therefore the same transaction:

```sql
UPDATE donat.file_uploads SET state = 'claimed', claimed_at = now()
 WHERE id = <submitted> AND state = 'pending' AND expires_at > now()
   AND byte_size > 0 AND attachment = 'public.pet.photo'
   AND session_role = <session role>
   AND session_key IS NOT DISTINCT FROM <session key>
```

If the gate updates no row the mutation fails as `validation-failed`, with one
message for every cause — unknown id, already claimed, expired, another
attachment, another session. The write never lands, so a column can never point
at an upload that was not verified.

Verification of what was actually stored happens before the row moves to
`claimed`, and it is the `byte_size > 0` in the gate above: the completion call
records what a `HEAD` against the store reports, never what the client promised.
An upload nobody stored bytes for stays `pending`, cannot be claimed, and is
collected.

A presigned `PUT` cannot be revoked and stays valid for its whole lifetime, so
the bytes are not left where it writes: completion copies the verified object to
its final key and drops the staging one (§4). Anything written to the staging
key afterwards is an orphan nothing references.

Updating the column to another id claims the new upload and orphans the old
one; the collector reclaims the old bytes after `orphan_grace_days`.

## 6. Reading

A declared file column projects an object instead of a bare uuid:

```graphql
query { pet { photo { id file_name media_type size url } } }
```

`url` is the signed download URL, built in SQL (§3): a `/v1/files/{id}?…`
a presigned `GET` at the store. It is produced only for rows the role's select
permission already returned, and it expires after `download_ttl_seconds`.

The projection joins an upload that is `claimed` **and** belongs to this very
column. The claim gate is the intended way a value gets into the column, but it
is not the only way one could — a migration, a command step, or a repair script
can write any uuid — and a signed URL must never be minted for an upload nobody
verified. A value that does not qualify reads as NULL, exactly like an unset
attachment.

The engine serves a download with `X-Content-Type-Options: nosniff` and
`Content-Security-Policy: default-src 'none'; sandbox`. The bytes and their
declared type both come from a caller and are served from the API's own origin,
and `media_types` is optional, so the response cannot rely on the allow-list to
keep an uploaded HTML or SVG file from becoming script on that origin.

A stored file never changes: its id is consumed once and its bytes are never
rewritten. The only reason its URL can differ between two reads is that the
signature would otherwise expire, and the engine makes that the only reason — it
signs as of `now` floored to half `download_ttl_seconds`, so every read inside a
window returns byte-identical bytes and a URL minted at the end of one still has
at least half its lifetime left.

This is not only about caching. A subscription re-runs its query on a timer and
pushes whenever the response differs (`crates/server/src/ws.rs`), so a URL
carrying the current second would make every poll look like a change and every
subscription on a row with a file would fire forever. As it is, such a
subscription pushes at most once per `download_ttl_seconds / 2` on an unchanged
row — and a deployment that wants that rarer raises the TTL, which is the same
knob that decides how long a leaked URL stays usable.

The store verifies the signature and expiry itself. Nobody performs a further
permission check: the signature *is* the capability, issued by a query that had
already passed the row's select permission. That is the presigned-URL trust
model, and choosing it is what keeps the bytes off the engine's request path
entirely.

## 7. Collection

One background task, started only when some table declares an attachment, and
modeled on `cron.rs`. It wakes every `gc.every_days` (default 1) and, per
declaration, in one statement each:

1. `pending` rows past `expires_at + pending_ttl_days` — never claimed;
2. `claimed` rows older than `orphan_grace_days` that no row of the owning
   table still references (`NOT EXISTS (SELECT 1 FROM public.pet WHERE photo = f.id)`).

Rows are claimed in batches of 500 with `FOR UPDATE SKIP LOCKED`, and each
sweep repeats until it drains: a pass that stopped after one batch would reclaim
at most 500 objects per interval, which a caller can out-create by orders of
magnitude. The object is deleted from storage (`unlink`, or a SigV4-signed
`DELETE`), and only a successful delete — or a not-found — removes the catalog
row, so a storage failure retries on the next pass instead of leaking a forgotten
object. Deleting is the only file I/O the engine performs outside a request.

## 8. Operating them

Attachments are the first feature whose state lives in two places at once, and
that changes what a backup is: a `pg_dump` alone is no longer a complete one. A
restored `donat.file_uploads` row whose object is gone serves a 404, and an
object whose row is gone is unreachable *and* uncollectable — the collector
finds orphans through rows that still exist. Back up the object store together
with the database, and restore the store from a point at or after the dump: an
object with no row is a leak, but a row with no object is a broken link.

Every file route logs one event under `donat::files` with the route, the upload
id and the status, so a forged signature, an oversize body and an unreachable
store are distinguishable in a deployment's logs.

## 9. Tests

- `crates/metadata` — loading, defaults, and every refusal in §1.
- `crates/sqlgen` — insta snapshots for the claim gate, the file-column
  projection, and the minting statement.
- `crates/storage` — the signing primitives, with the published AWS presigned
  example pinned as a vector so the SQL half has something to be checked against.
- `crates/conformance` (`tests/file_attachments.rs`) — the round trip through the
  spawned binary against a **real MinIO** from
  `docker-compose.conformance.yml`: request → upload → complete → claim → read →
  download; a tampered and an unsigned URL both refused *by the store*; a body of
  another length refused by the store; a claim refused for each cause in §5; a
  late write to the upload URL that cannot reach the claimed bytes; a public file
  read anonymously; a session's budget; URL stability; a collector pass that
  removes an expired pending upload and an orphaned object and leaves a live one
  alone; and a deployment without attachments where neither the root field nor
  the route exists.

  A hand-written stub could only prove the signature is canonical by our own
  reading of the spec. Switching to MinIO immediately caught one that was not:
  the `X-Amz-SignedHeaders` separator has to be percent-encoded, because Go's
  URL parser rejects a query containing a raw `;` outright.
