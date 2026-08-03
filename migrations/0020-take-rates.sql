CREATE TABLE take_rates (
  id BIGSERIAL NOT NULL,
  vote_account TEXT NOT NULL,
  epoch NUMERIC NOT NULL,
  validator_rewards NUMERIC NOT NULL,
  total_rewards NUMERIC NOT NULL,
  take_rate DOUBLE PRECISION NOT NULL,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL,
  updated_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(id),
  UNIQUE(vote_account, epoch)
);

CREATE INDEX idx_take_rates_epoch
    ON take_rates(epoch);
CREATE INDEX idx_take_rates_vote_account
    ON take_rates(vote_account);
