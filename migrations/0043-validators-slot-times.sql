CREATE TABLE validators_slot_times (
    epoch NUMERIC NOT NULL,
    vote_account TEXT NOT NULL,
    source TEXT NOT NULL,
    slot_duration_median_ms DOUBLE PRECISION NOT NULL,
    ibrl_score DOUBLE PRECISION,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL,
    updated_at TIMESTAMP WITH TIME ZONE NOT NULL,
    PRIMARY KEY (epoch, vote_account, source)
);

CREATE INDEX idx_validators_slot_times_epoch ON validators_slot_times(epoch);
