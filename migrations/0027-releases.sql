-- One row per (client_lineage, client_version). Three fetchers fill it and they write disjoint
-- columns: GitHub releases give availability, the SFDP endpoint and the feature gate tracker each
-- give a floor.
-- Mainnet only, like every other table here.
CREATE TABLE releases (
  id BIGSERIAL NOT NULL,
  -- agave, frankendancer, firedancer, sig
  client_lineage TEXT NOT NULL,
  -- As the client reports it in gossip, e.g. 4.2.2, 4.3.0-rc.0, 0.1106.40201, 26.8.2
  client_version TEXT NOT NULL,
  -- Publish time; the epoch it falls in is resolved against the epochs table when read.
  released_at TIMESTAMP WITH TIME ZONE NULL,
  release_url TEXT NULL,
  -- First epoch the Solana Foundation Delegation Program required this version
  sfdp_floor_epoch NUMERIC NULL,
  -- First epoch the cluster's feature gates required this version
  feature_gate_epoch NUMERIC NULL,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL,
  updated_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(id),
  UNIQUE(client_lineage, client_version)
);

-- The feature-gate floor history, reconstructed from the Solana Tech Discord announcements
-- (epochs 943-1019) and from the gates' own activation slots for the rest. The collector derives
-- the same timeline from Anza's tracker, but only over the newest gates it reads, so this is what
-- gives the older epochs an answer.
INSERT INTO releases (client_lineage, client_version, feature_gate_epoch, created_at, updated_at)
VALUES
  ('agave',         '3.1.0',          946, NOW(), NOW()),
  ('agave',         '3.1.7',          953, NOW(), NOW()),
  ('agave',         '4.0.0-beta.0',   979, NOW(), NOW()),
  ('agave',         '4.0.2',          992, NOW(), NOW()),
  ('agave',         '4.1.0-beta.0',   999, NOW(), NOW()),
  ('agave',         '4.1.0-beta.1',  1008, NOW(), NOW()),
  ('agave',         '4.2.0-beta.1',  1019, NOW(), NOW()),
  ('frankendancer', '0.812.30108',    946, NOW(), NOW()),
  ('frankendancer', '0.902.40002',    979, NOW(), NOW()),
  ('frankendancer', '0.911.40002',    992, NOW(), NOW()),
  ('frankendancer', '0.1001.40101',   999, NOW(), NOW()),
  ('frankendancer', '0.1102.40201',  1019, NOW(), NOW()),
  ('firedancer',    '1.1.1',         1019, NOW(), NOW()),
  ('firedancer',    '26.8.0',        1026, NOW(), NOW());
