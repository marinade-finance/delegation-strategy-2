-- Rows are never deleted: removed_epoch closes a row, so the table keeps who was listed when and why.
CREATE TABLE blacklist (
    id BIGSERIAL PRIMARY KEY,
    vote_account TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('manual', 'legacy', 'auction')),
    code TEXT NOT NULL,
    reason TEXT,
    -- Auction rows: the SAM auction that listed the account and its run in ds-sam-pipeline.
    auction_epoch NUMERIC,
    run_id TEXT,
    added_epoch NUMERIC,
    removed_epoch NUMERIC,
    removed_reason TEXT,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL,
    updated_at TIMESTAMP WITH TIME ZONE NOT NULL,
    CHECK ((source = 'auction') = (auction_epoch IS NOT NULL AND run_id IS NOT NULL))
);

CREATE UNIQUE INDEX blacklist_active_manual ON blacklist(vote_account, code)
    WHERE source = 'manual' AND removed_epoch IS NULL;
CREATE UNIQUE INDEX blacklist_active_auction ON blacklist(vote_account, code)
    WHERE source = 'auction' AND removed_epoch IS NULL;

-- The exemptions of the config, served to the auction so it never lists an exempted account.
CREATE TABLE blacklist_exempt (
    vote_account TEXT NOT NULL,
    rule TEXT NOT NULL,
    until_epoch NUMERIC,
    PRIMARY KEY (vote_account, rule)
);

-- The ds-sam-pipeline run whose listings each auction epoch holds; a newer run of the epoch replaces them.
CREATE TABLE blacklist_auction_imports (
    auction_epoch NUMERIC PRIMARY KEY,
    run_id TEXT NOT NULL,
    rows INTEGER NOT NULL,
    imported_at TIMESTAMP WITH TIME ZONE NOT NULL
);
