-- Tower vote credits only: NULL for an Alpenglow epoch.
ALTER TABLE validators ALTER COLUMN credits DROP NOT NULL;

ALTER TABLE validators
  -- NULL for epochs stored before this column existed. On testnet, those rows from epoch 1042 on hold lamports in credits.
  ADD COLUMN credits_regime TEXT DEFAULT NULL CHECK (credits_regime IN ('tower', 'migration', 'alpenglow')),
  -- Lamports, not vote credits.
  ADD COLUMN alpenglow_credits NUMERIC DEFAULT NULL,
  -- JSON text of getVoteAccounts epochCredits as [[epoch, credits, prev_credits], ...], with the [u64::MAX, u64::MAX, u64::MAX] migration marker.
  ADD COLUMN epoch_credits_raw TEXT DEFAULT NULL;

-- Cumulative epochCredits of the vote account at the last snapshot.
ALTER TABLE uptimes ADD COLUMN last_credits NUMERIC DEFAULT NULL;
