-- NULL: not collected, or the vote state did not parse. Rows before this migration stay NULL.
ALTER TABLE validators ADD COLUMN bls_pubkey TEXT DEFAULT NULL;
-- SIMD-0357 verdict for the inflation rewards of this epoch, from the last snapshot before close_epoch.
ALTER TABLE validators ADD COLUMN vat_admitted BOOLEAN DEFAULT NULL;
-- no_bls_key, no_stake, low_balance or below_stake_cutoff. NULL when admitted.
ALTER TABLE validators ADD COLUMN vat_admission_reason TEXT DEFAULT NULL;
