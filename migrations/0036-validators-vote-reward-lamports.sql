-- Tower vote credits only: NULL for an Alpenglow epoch.
ALTER TABLE validators ALTER COLUMN credits DROP NOT NULL;

ALTER TABLE validators ADD COLUMN vote_reward_lamports NUMERIC DEFAULT NULL;

-- Cumulative epochCredits of the vote account at the last snapshot.
ALTER TABLE uptimes ADD COLUMN last_credits NUMERIC DEFAULT NULL;
