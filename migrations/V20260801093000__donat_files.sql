-- File attachments (spec 008): the upload catalog and the SQL half of URL
-- signing.
--
-- Deploy-time only, like every other migration here: the serving binary never
-- runs DDL. It inserts, claims, and collects rows in `donat.file_uploads`, and
-- calls `donat.s3_presigned_url` from inside the one statement it already emits
-- per operation. Signing lives in SQL precisely so that a query can return a
-- signed URL per row without the engine walking the response in Rust (the M4
-- one-statement invariant).

create extension if not exists pgcrypto;

-- One row per requested upload. `state` moves 'pending' -> 'claimed' exactly
-- once, in the same statement that writes the owning column, so a column value
-- can never point at bytes nobody verified. It never moves back.
create table if not exists donat.file_uploads (
    id             uuid primary key default gen_random_uuid(),
    -- '<schema>.<table>.<column>' of the declaring attachment.
    attachment     text        not null,
    backend        text        not null,
    object_key     text        not null,
    file_name      text        not null,
    media_type     text        not null,
    declared_bytes bigint      not null,
    -- What storage actually holds, recorded when the bytes arrive. NULL until
    -- then, which is why an unverified upload can never be claimed.
    byte_size      bigint,
    -- pending | claimed
    state          text        not null default 'pending',
    -- The role and session that asked for the URL; only they may claim it.
    session_role   text        not null,
    session_key    text,
    created_at     timestamptz not null default now(),
    expires_at     timestamptz not null,
    claimed_at     timestamptz,
    unique (backend, object_key)
);

-- The collector's two sweeps: expired pending uploads, and claimed rows old
-- enough to be checked for references.
create index if not exists file_uploads_pending_idx
    on donat.file_uploads (state, expires_at);
create index if not exists file_uploads_claimed_idx
    on donat.file_uploads (attachment, state, claimed_at);

-- An AWS SigV4 presigned URL for one object.
--
-- Everything constant for the statement is computed in Rust and passed in:
-- `k_signing` is the full kDate -> kRegion -> kService -> kSigning chain
-- (cached per UTC day), `credential_encoded` is already percent-encoded, and
-- `origin`/`host`/`canonical_uri` already account for path-style buckets. What
-- remains per row is the object key, so this function is the only part that
-- has to run once per returned row.
--
-- `canonical_uri` needs no escaping because object keys are engine-chosen and
-- restricted to unreserved characters plus '/'.
create or replace function donat.s3_presigned_url(
    k_signing          bytea,
    credential_encoded text,
    scope              text,
    amz_date           text,
    expires            int,
    origin             text,
    host               text,
    canonical_uri      text,
    method             text
)
returns text
language sql
immutable
strict
parallel safe
as $$
    with q as (
        select 'X-Amz-Algorithm=AWS4-HMAC-SHA256'
            || '&X-Amz-Credential=' || credential_encoded
            || '&X-Amz-Date=' || amz_date
            || '&X-Amz-Expires=' || expires::text
            -- Host is the only signed header here, so there is no ';' to
            -- percent-encode; the Rust signer, which binds more, encodes it.
            || '&X-Amz-SignedHeaders=host' as query
    ),
    canonical as (
        select query,
               method || E'\n' || canonical_uri || E'\n' || query || E'\n'
                   || 'host:' || host || E'\n' || E'\n'
                   || 'host' || E'\n'
                   || 'UNSIGNED-PAYLOAD' as request
        from q
    ),
    signed as (
        select query,
               encode(
                   hmac(
                       convert_to(
                           'AWS4-HMAC-SHA256' || E'\n' || amz_date || E'\n' || scope || E'\n'
                               || encode(digest(convert_to(request, 'UTF8'), 'sha256'), 'hex'),
                           'UTF8'
                       ),
                       k_signing, 'sha256'
                   ),
                   'hex'
               ) as signature
        from canonical
    )
    select origin || canonical_uri || '?' || query || '&X-Amz-Signature=' || signature
    from signed
$$;
