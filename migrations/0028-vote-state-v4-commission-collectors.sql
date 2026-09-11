-- Vote state v4 (SIMD-0185) commission and collector fields, sampled from raw vote accounts.
-- Names and null semantics match solana-snapshot-parser's ValidatorMeta so this derivation and the
-- one stakes-etl loads into BigQuery stay reconcilable.
--
-- Basis points are the authoritative rate; commission_advertised / commission_effective /
-- commission_max_observed / commission_min_observed stay whole-percent projections of it.
--
-- Nothing here backfills commission_effective, and from epoch 1030 on nothing can: the epoch-1030
-- payout landed at the SIMD-0232 activation slot, which removed commission and commissionBps from
-- the reward rows, so 1030 and 1031 are NULL fleet-wide. Recovering the applied rate would need the
-- sum of every delegator's stake reward for the epoch, which is not an RPC query; stakes-etl
-- reconstructs it from a snapshot instead. Epochs up to 1029 are populated, and the earlier
-- SIMD-0291 gap was backfilled from reward rows under GEN-8658 while the field still existed.
ALTER TABLE validators
  -- NULL on a pre-v4 vote state, never a zeroed pubkey: agave has no collector to pay there and
  -- credits the vote account (inflation) or the leader identity (block revenue) instead.
  ADD COLUMN inflation_rewards_collector TEXT DEFAULT NULL,
  ADD COLUMN block_revenue_collector TEXT DEFAULT NULL,
  -- Set on every version: agave synthesizes percent * 100 on a pre-v4 state, so this is the rate it
  -- applies either way. is_v4 is what says whether the validator set it in basis points itself.
  ADD COLUMN inflation_rewards_commission_bps INTEGER DEFAULT NULL,
  ADD COLUMN inflation_rewards_commission_bps_is_v4 BOOLEAN DEFAULT NULL,
  -- Inert until SIMD-0123 activates; defaults to 10000 on migration to v4.
  ADD COLUMN block_revenue_commission_bps INTEGER DEFAULT NULL,
  ADD COLUMN pending_delegator_rewards NUMERIC DEFAULT NULL,
  -- 'reward_row' where the runtime told us the rate it applied, 'vote_state' where SIMD-0232 left
  -- only the sampled state to go on. NULL for epochs closed before this column existed and for a
  -- validator with neither source, which is not the same as a rate of zero.
  ADD COLUMN commission_effective_source TEXT DEFAULT NULL;
