-- Nullable on purpose: rows written before the gate read carry no observation, and the running
-- epoch has no `epochs` row yet, so this is the only place its slot-time regime is recorded.
ALTER TABLE cluster_info ADD COLUMN slots_per_year DOUBLE PRECISION NULL;
