CREATE TABLE vat_verdict_checks (
    epoch NUMERIC PRIMARY KEY,
    checked_at TIMESTAMP WITH TIME ZONE NOT NULL,
    mismatched INTEGER NOT NULL,
    only_bigquery TEXT[] NOT NULL,
    only_stored TEXT[] NOT NULL
);
