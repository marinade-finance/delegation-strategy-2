ALTER TABLE validators
  ADD COLUMN inflation_rewards_collector_owner TEXT DEFAULT NULL,
  ADD COLUMN inflation_rewards_collector_lamports NUMERIC DEFAULT NULL,
  ADD COLUMN inflation_rewards_collector_healthy BOOLEAN DEFAULT NULL,
  ADD COLUMN block_revenue_collector_owner TEXT DEFAULT NULL,
  ADD COLUMN block_revenue_collector_lamports NUMERIC DEFAULT NULL,
  ADD COLUMN block_revenue_collector_healthy BOOLEAN DEFAULT NULL;
