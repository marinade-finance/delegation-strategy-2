CREATE TABLE jito_priority_fee (
  id BIGSERIAL NOT NULL,
  vote_account TEXT NOT NULL,
  validator_commission INTEGER NOT NULL,
  total_lamports_transferred NUMERIC NOT NULL,
  total_epoch_rewards NUMERIC,
  claimed_epoch_rewards NUMERIC,
  total_epoch_claimants INTEGER,
  epoch_active_claimants INTEGER,
  epoch_slot NUMERIC NOT NULL,
  epoch INTEGER NOT NULL,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(id)
)
