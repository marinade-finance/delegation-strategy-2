-- Re-resolves closed epochs at close_epoch's vintage; an advertised percent stands in where a row has no bps.
WITH resolved AS (
  SELECT
    v.vote_account,
    v.epoch,
    CASE
      WHEN p2.inflation_rewards_commission_bps IS NOT NULL THEN p2.inflation_rewards_commission_bps
      WHEN p2.commission_advertised IS NOT NULL THEN p2.commission_advertised * 100
      WHEN p1.inflation_rewards_commission_bps IS NOT NULL THEN p1.inflation_rewards_commission_bps
      WHEN p1.commission_advertised IS NOT NULL THEN p1.commission_advertised * 100
      WHEN v.inflation_rewards_commission_bps IS NOT NULL THEN v.inflation_rewards_commission_bps
      ELSE v.commission_advertised * 100
    END AS rate_bps,
    CASE
      WHEN p2.inflation_rewards_commission_bps IS NOT NULL THEN TRUE
      WHEN p2.commission_advertised IS NOT NULL THEN FALSE
      WHEN p1.inflation_rewards_commission_bps IS NOT NULL THEN TRUE
      WHEN p1.commission_advertised IS NOT NULL THEN FALSE
      ELSE v.inflation_rewards_commission_bps IS NOT NULL
    END AS from_bps
  FROM validators v
  LEFT JOIN validators p2 ON p2.vote_account = v.vote_account AND p2.epoch = v.epoch - 2
  LEFT JOIN validators p1 ON p1.vote_account = v.vote_account AND p1.epoch = v.epoch - 1
  WHERE v.epoch >= 1030
    AND v.epoch IN (SELECT epoch FROM epochs)
    AND (v.commission_effective_source IS NULL OR v.commission_effective_source = 'vote_state')
)
UPDATE validators
SET
  commission_effective = CEIL(LEAST(resolved.rate_bps, 10000) / 100.0)::INTEGER,
  commission_effective_bps = CASE WHEN resolved.from_bps THEN resolved.rate_bps END,
  commission_effective_source = 'vote_state'
FROM resolved
WHERE validators.vote_account = resolved.vote_account
  AND validators.epoch = resolved.epoch
  AND resolved.rate_bps IS NOT NULL;

UPDATE validators
SET
  commission_max_observed = GREATEST(
    (SELECT MAX(commission) FROM commissions c WHERE c.vote_account = validators.vote_account AND c.epoch = validators.epoch),
    commission_advertised,
    commission_effective
  ),
  commission_min_observed = LEAST(
    (SELECT MIN(commission) FROM commissions c WHERE c.vote_account = validators.vote_account AND c.epoch = validators.epoch),
    commission_advertised,
    commission_effective
  )
WHERE epoch >= 1030
  AND epoch IN (SELECT epoch FROM epochs)
  AND (commission_effective_source IS NULL OR commission_effective_source = 'vote_state');
