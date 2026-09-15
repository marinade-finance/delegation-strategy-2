-- Names and null semantics match solana-snapshot-parser's ValidatorMeta so both stay reconcilable.
ALTER TABLE validators
  -- NULL on a pre-v4 state: agave credits the vote account or leader identity, with no collector.
  ADD COLUMN inflation_rewards_collector TEXT DEFAULT NULL,
  ADD COLUMN block_revenue_collector TEXT DEFAULT NULL,
  -- Set on every version; is_v4 is what says the validator chose basis points itself.
  ADD COLUMN inflation_rewards_commission_bps INTEGER DEFAULT NULL,
  ADD COLUMN inflation_rewards_commission_bps_is_v4 BOOLEAN DEFAULT NULL,
  -- Inert until SIMD-0123 activates; agave defaults it to 10000 on migration to v4.
  ADD COLUMN block_revenue_commission_bps INTEGER DEFAULT NULL,
  ADD COLUMN pending_delegator_rewards NUMERIC DEFAULT NULL,
  -- NULL for epochs closed before this column existed, which is not the same as a rate of zero.
  ADD COLUMN commission_effective_source TEXT DEFAULT NULL;
