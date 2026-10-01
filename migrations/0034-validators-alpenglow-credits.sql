-- Tower vote credits only: NULL for an Alpenglow epoch.
ALTER TABLE validators ALTER COLUMN credits DROP NOT NULL;

-- Lamports, not vote credits.
ALTER TABLE validators ADD COLUMN alpenglow_credits NUMERIC DEFAULT NULL;

-- Cumulative epochCredits of the vote account at the last snapshot.
ALTER TABLE uptimes ADD COLUMN last_credits NUMERIC DEFAULT NULL;
