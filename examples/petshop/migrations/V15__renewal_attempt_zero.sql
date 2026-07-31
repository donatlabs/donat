-- The renewal Process numbers its authorization attempts from zero: attempt 0
-- is the scheduled cycle itself, and 1 and 2 are the dunning retries. The audit
-- records every attempt, including the first, so the original `attempt > 0`
-- check made the initial outcome unrecordable and wedged every renewal that
-- reached `record_initial_renewal`.
ALTER TABLE subscription_dunning_attempt
  DROP CONSTRAINT subscription_dunning_attempt_attempt_check;
ALTER TABLE subscription_dunning_attempt
  ADD CONSTRAINT subscription_dunning_attempt_attempt_check CHECK (attempt >= 0);
