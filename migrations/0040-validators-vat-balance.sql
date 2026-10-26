-- NULL on rows stored before this migration.
ALTER TABLE validators ADD COLUMN vote_account_lamports NUMERIC DEFAULT NULL;
-- Rent of a v4 vote state plus the VAT of the next epoch. NULL on rows stored before this
-- migration and on runs for a backfilled epoch.
ALTER TABLE cluster_info ADD COLUMN vat_minimum_balance NUMERIC DEFAULT NULL;
