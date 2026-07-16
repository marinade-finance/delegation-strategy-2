CREATE TABLE validators_stakers (
  id BIGSERIAL NOT NULL,
  epoch NUMERIC NOT NULL,
  vote_account TEXT NOT NULL,
  unique_stakers NUMERIC NOT NULL,
  active_stake NUMERIC NOT NULL,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL,
  updated_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(id),
  UNIQUE(epoch, vote_account)
);

CREATE INDEX idx_validators_stakers_epoch
    ON validators_stakers(epoch);
