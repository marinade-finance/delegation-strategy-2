-- NULL for a vote account without stake and until store vat-verdict reads the payout of the epoch.
ALTER TABLE validators ADD COLUMN vat_admitted BOOLEAN DEFAULT NULL;
