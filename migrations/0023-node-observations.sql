-- Keyed by identity, not vote_account: gossip returns identity natively and most nodes on the
-- cluster have no vote account at all, so a vote-account key would drop them permanently.
CREATE TABLE node_observations (
  id BIGSERIAL NOT NULL,
  identity TEXT NOT NULL,
  ip TEXT NULL,
  gossip_port INTEGER NULL,
  version TEXT NULL,
  client_id INTEGER NULL,
  client_id_raw TEXT NULL,
  feature_set BIGINT NULL,
  shred_version INTEGER NULL,
  rpc_public BOOLEAN NULL,
  pubsub_public BOOLEAN NULL,
  epoch_slot NUMERIC NOT NULL,
  epoch NUMERIC NOT NULL,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL,
  -- A row is written only when something changed, so created_at cannot answer "is this node still
  -- here". Every run re-stamps this on the node's newest row, making each row an observed interval.
  last_seen_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(id)
);

CREATE INDEX idx_node_observations_identity_created_at
    ON node_observations(identity, created_at);
CREATE INDEX idx_node_observations_ip_last_seen_at
    ON node_observations(ip, last_seen_at);
