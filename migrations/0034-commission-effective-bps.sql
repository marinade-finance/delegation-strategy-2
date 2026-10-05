-- NULL where the rate came from a reward row or from commission_advertised, both whole percent.
ALTER TABLE validators ADD COLUMN commission_effective_bps INTEGER DEFAULT NULL;
