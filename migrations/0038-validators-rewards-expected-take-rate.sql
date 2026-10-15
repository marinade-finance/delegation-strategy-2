-- Written by `store` once the epoch's rewards, commissions and Jito rows are stored; NULL until then.
ALTER TABLE validators_rewards ADD COLUMN expected_take_rate DOUBLE PRECISION NULL;
