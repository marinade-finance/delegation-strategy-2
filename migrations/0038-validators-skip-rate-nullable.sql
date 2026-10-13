-- NULL when no leader slot of the vote account is resolved.
ALTER TABLE validators ALTER COLUMN skip_rate DROP NOT NULL;
