-- Rows are never deleted: removed_epoch closes a row, so the table keeps who was listed when and why.
CREATE TABLE blacklist (
    id BIGSERIAL PRIMARY KEY,
    vote_account TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('manual', 'rule', 'legacy')),
    rule TEXT CHECK ((source = 'rule') = (rule IS NOT NULL)),
    code TEXT NOT NULL,
    reason TEXT,
    evidence JSONB,
    -- Rule rows: the earliest epoch the evidence covers, which an exemption's until_epoch is held against.
    evidence_from_epoch NUMERIC,
    added_epoch NUMERIC,
    removed_epoch NUMERIC,
    removed_reason TEXT,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL,
    updated_at TIMESTAMP WITH TIME ZONE NOT NULL
);

CREATE UNIQUE INDEX blacklist_active_manual ON blacklist(vote_account, code)
    WHERE source = 'manual' AND removed_epoch IS NULL;
CREATE UNIQUE INDEX blacklist_active_rule ON blacklist(vote_account, rule)
    WHERE source = 'rule' AND removed_epoch IS NULL;

CREATE TABLE blacklist_evaluations (
    epoch NUMERIC PRIMARY KEY,
    rows_added INTEGER NOT NULL,
    -- The epoch's inputs never arrived; a later epoch was judged instead.
    skipped BOOLEAN NOT NULL DEFAULT false,
    evaluated_at TIMESTAMP WITH TIME ZONE NOT NULL
);
