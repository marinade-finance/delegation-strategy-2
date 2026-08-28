-- One row per collect snapshot in both tables, so a per-epoch read picks the newest per (vote_account, epoch).
CREATE INDEX idx_mev_vote_account_epoch ON mev(vote_account, epoch, created_at DESC);
CREATE INDEX idx_jito_priority_fee_vote_account_epoch ON jito_priority_fee(vote_account, epoch, created_at DESC);
