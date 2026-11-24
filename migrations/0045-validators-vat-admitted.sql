-- NULL until store vat-verdict reads the payout of the epoch, and for a vote account that paid no VAT and has no stake or row in the next epoch.
ALTER TABLE validators ADD COLUMN vat_admitted BOOLEAN DEFAULT NULL;
