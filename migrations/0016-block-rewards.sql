CREATE TABLE validators_block_rewards (
  id BIGSERIAL NOT NULL,
  epoch NUMERIC NOT NULL,
  identity_account TEXT NOT NULL,
  vote_account TEXT NOT NULL,
  authorized_voter TEXT NOT NULL,
  amount NUMERIC NOT NULL,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL,
  updated_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(id),
  UNIQUE(epoch, identity_account, vote_account)
);

CREATE INDEX idx_validators_block_rewards_epoch
    ON validators_block_rewards(epoch);
