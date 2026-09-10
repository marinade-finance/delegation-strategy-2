-- One row per (client_lineage, client_version). Two collectors fill it and they write disjoint
-- columns: GitHub releases give availability, the SFDP endpoint gives the floor.
-- Mainnet only, like every other table here.
-- The cluster's own feature-gate floors are static data, in store/release_feature_gates.csv.
CREATE TABLE releases (
  id BIGSERIAL NOT NULL,
  -- agave, frankendancer, firedancer, sig
  client_lineage TEXT NOT NULL,
  -- As the client reports it in gossip, e.g. 4.2.2, 4.3.0-rc.0, 0.1106.40201, 26.8.2
  client_version TEXT NOT NULL,
  -- Epoch the release was published in
  available_epoch NUMERIC NULL,
  released_at TIMESTAMP WITH TIME ZONE NULL,
  release_url TEXT NULL,
  -- First epoch the Solana Foundation Delegation Program required this version
  sfdp_floor_epoch NUMERIC NULL,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL,
  updated_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(id),
  UNIQUE(client_lineage, client_version)
);

CREATE INDEX idx_releases_lineage_available_epoch
    ON releases(client_lineage, available_epoch DESC);
-- Serves "which floor was in force at epoch N": the rows without a floor are the majority and
-- never answer it.
CREATE INDEX idx_releases_lineage_sfdp_floor_epoch
    ON releases(client_lineage, sfdp_floor_epoch DESC)
    WHERE sfdp_floor_epoch IS NOT NULL;
