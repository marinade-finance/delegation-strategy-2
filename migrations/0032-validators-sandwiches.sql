CREATE TABLE validators_sandwiches (
  epoch NUMERIC NOT NULL,
  vote_account TEXT NOT NULL,
  blocks_produced NUMERIC NOT NULL,
  blocks_with_sandwiches NUMERIC NOT NULL,
  sandwich_rate_30d DOUBLE PRECISION NOT NULL,
  -- Absent before epoch 820: upstream published only the 30d rate then.
  sandwich_rate_60d DOUBLE PRECISION,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL,
  updated_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(epoch, vote_account)
);

-- For reading sandwich incidents by epoch
CREATE INDEX idx_validators_sandwiches_epoch
    ON validators_sandwiches(epoch);
