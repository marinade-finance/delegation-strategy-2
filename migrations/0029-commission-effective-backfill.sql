-- SIMD-0232 dropped the rate from reward rows at 1030; close_epoch samples vote state after this.
UPDATE validators
SET commission_effective = commission_advertised
WHERE epoch >= 1030
  AND commission_effective IS NULL
  AND epoch IN (SELECT epoch FROM epochs);
