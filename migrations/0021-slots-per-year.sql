ALTER TABLE epochs ADD COLUMN slots_per_year DOUBLE PRECISION NULL;

-- Agave's pre-SIMD-0525 baseline, which applied to every epoch collected so far. Within 0.002% of
-- the 365.25/2 the inflation formulas used until now, so no historical figure visibly moves.
UPDATE epochs SET slots_per_year = 78892314.984;

ALTER TABLE epochs ALTER COLUMN slots_per_year SET NOT NULL;
