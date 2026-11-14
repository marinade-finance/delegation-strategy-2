-- NULL until close_epoch reads the VAT debits of the epoch.
ALTER TABLE validators ADD COLUMN vat_admitted BOOLEAN DEFAULT NULL;
