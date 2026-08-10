-- OAuth2 authorization-code credentials (spec 011).
--
-- Every other Donat credential is a `SecretRef` read from an environment
-- variable at boot: immutable, read-only, never written back. A refresh token
-- breaks that shape, because the engine must store a value it obtained at
-- runtime and, for providers that rotate on refresh, replace it atomically or
-- lose the account. This table is that one exception and nothing else.
--
-- It is source-local, like the Process journal that uses the connector
-- instance, and it is written by exactly two things: the deploy-time
-- `donat connector authorize` command, and the refresh-at-use path inside an
-- activity attempt. No HTTP surface reads, writes, or lists it.
--
-- `access_token` and `refresh_token` are sealed with AES-256-GCM before they
-- reach this table (see crates/server/src/credentials/seal.rs). The stored
-- bytes are `nonce || ciphertext || tag`, and the additional authenticated
-- data binds the row to `source | connector | instance | subject |
-- token_origin`, so a sealed value lifted out of one row cannot be opened in
-- another. The database never sees a token in the clear, which is also why
-- there is no index, constraint, or generated column over these two columns:
-- nothing here may depend on their contents.
--
-- `subject` is the provider's own account/tenant identifier, recorded so an
-- operator can tell two authorizations apart. It is never a Donat user
-- identity and never participates in a permission decision.

create table if not exists donat.connector_credential (
    -- Metadata source the connector instance belongs to. Present in the key
    -- (rather than implied by the database) because the same physical database
    -- may back more than one named source.
    source            text        not null,
    -- The connector module (`module:` in metadata), not the instance name.
    connector         text        not null,
    -- The deploy-time connector instance name (`name:` in metadata).
    instance          text        not null,
    -- Provider account identity. See the note above.
    subject           text        not null,

    access_token      bytea       not null,
    access_expires_at timestamptz not null,
    -- Null when the provider issued no refresh token: the credential is then
    -- usable until it expires and must be re-authorized by an operator.
    refresh_token     bytea,
    -- The scopes the provider actually granted, verified at authorize time to
    -- cover the declared set.
    scopes            text[]      not null,
    -- The token endpoint this credential was minted at. Part of the sealing
    -- AAD, so moving a row to an instance pointing somewhere else makes it
    -- unopenable rather than replayable.
    token_origin      text        not null,

    rotated_at        timestamptz not null default now(),
    rotation_count    bigint      not null default 0,
    created_at        timestamptz not null default now(),

    -- Set when the provider answered a refresh with `invalid_grant`. The row
    -- is kept, not deleted: an operator has to be able to see what happened
    -- and re-authorize. A row with a reason is never refreshed again, which is
    -- what stops the retry loop.
    unusable_reason   text,
    unusable_at       timestamptz,

    primary key (source, connector, instance, subject),

    constraint connector_credential_unusable_is_whole
        check ((unusable_reason is null) = (unusable_at is null))
);

-- The listing the `donat connector credentials list` command answers, and the
-- startup check that a configured instance actually has a credential, both
-- filter on `(source, connector, instance)`. That is a prefix of the primary
-- key, so its index already serves them; a second index over the same columns
-- would only cost writes.
