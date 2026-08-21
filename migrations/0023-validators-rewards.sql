CREATE TABLE validators_rewards (
  id BIGSERIAL NOT NULL,
  vote_account TEXT NOT NULL,
  epoch NUMERIC NOT NULL,
  validator_rewards NUMERIC NOT NULL,
  total_rewards NUMERIC NOT NULL,
  -- Components of total_rewards, both sides. They sum to total_rewards.
  inflation_rewards NUMERIC NOT NULL,
  mev_rewards NUMERIC NOT NULL,
  block_rewards NUMERIC NOT NULL,
  take_rate DOUBLE PRECISION NOT NULL,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL,
  updated_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(id),
  UNIQUE(vote_account, epoch)
);

CREATE INDEX idx_validators_rewards_epoch
    ON validators_rewards(epoch);
CREATE INDEX idx_validators_rewards_vote_account
    ON validators_rewards(vote_account);
