-- The only index on commissions is (identity, created_at), so every epoch-ranged read scans.
CREATE INDEX idx_commissions_epoch ON commissions (epoch);
