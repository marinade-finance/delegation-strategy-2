-- NULL for a vote account without stake that paid no VAT and until store vat-verdict reads the payout of the epoch.
ALTER TABLE validators ADD COLUMN vat_admitted BOOLEAN DEFAULT NULL;
