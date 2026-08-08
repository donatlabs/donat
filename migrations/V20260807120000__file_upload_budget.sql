-- Make the per-session upload budget hold against concurrency.
--
-- The budget was evaluated inside the minting statement, which takes an
-- advisory lock in a CTE and then counts the session's pending uploads. Under
-- READ COMMITTED a statement's snapshot is fixed before it begins executing —
-- that is, before the lock is acquired — so every concurrent caller counted the
-- same pre-lock state and every one of them passed. Fifty parallel requests
-- from one session all saw zero pending and all succeeded against a ceiling of
-- ten. The lock serialised them and changed nothing.
--
-- A PL/pgSQL function is the fix rather than a second statement, because the
-- engine renders one statement per operation. Each SQL statement inside a
-- PL/pgSQL body takes its own snapshot under READ COMMITTED, so the counts here
-- run *after* the lock and see what the previous holder committed.
--
-- VOLATILE, and it must stay so: marking it STABLE would let the planner
-- evaluate it once against the calling statement's snapshot, which is exactly
-- the bug this replaces.

CREATE OR REPLACE FUNCTION donat.file_upload_budget_ok(
    for_role text,
    for_key text,
    max_pending int,
    max_per_minute int
) RETURNS boolean
LANGUAGE plpgsql
VOLATILE
AS $$
BEGIN
    -- Scoped to this session's own key, so unrelated callers never wait on
    -- each other, and released when the transaction ends.
    PERFORM pg_advisory_xact_lock(
        hashtext('donat.file_uploads:' || for_role || ':' || coalesce(for_key, '')));

    RETURN (SELECT count(*) FROM donat.file_uploads b
            WHERE b.session_role = for_role
              AND b.session_key IS NOT DISTINCT FROM for_key
              AND b.state = 'pending'
              AND b.expires_at > now()) < max_pending
       AND (SELECT count(*) FROM donat.file_uploads b
            WHERE b.session_role = for_role
              AND b.session_key IS NOT DISTINCT FROM for_key
              AND b.created_at > now() - interval '1 minute') < max_per_minute;
END;
$$;
