ALTER TABLE mev
ALTER COLUMN total_epoch_rewards SET NOT NULL,
ALTER COLUMN claimed_epoch_rewards SET NOT NULL,
ALTER COLUMN total_epoch_claimants SET NOT NULL,
ALTER COLUMN epoch_active_claimants SET NOT NULL;
