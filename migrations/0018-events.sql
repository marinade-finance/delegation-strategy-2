CREATE TABLE validators_events (
  id BIGSERIAL NOT NULL,
  epoch NUMERIC NOT NULL,
  vote_account TEXT NOT NULL,
  reason TEXT NOT NULL,
  meta TEXT NOT NULL,
  amount NUMERIC NOT NULL,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL,
  updated_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(id),
  UNIQUE(epoch, vote_account, reason, meta)
);

CREATE INDEX idx_validators_events_epoch
    ON validators_events(epoch);
CREATE INDEX idx_validators_events_vote_account
    ON validators_events(vote_account);
