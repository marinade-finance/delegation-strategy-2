-- Written by `store` once the epoch's rewards, commissions and Jito rows are stored; NULL until then.
ALTER TABLE validators_rewards ADD COLUMN IF NOT EXISTS expected_take_rate DOUBLE PRECISION NULL;
