-- NULL when no leader slot of the vote account is resolved.
ALTER TABLE validators ALTER COLUMN skip_rate DROP NOT NULL;

UPDATE validators SET skip_rate = NULL WHERE leader_slots = 0 AND skip_rate = 0;
