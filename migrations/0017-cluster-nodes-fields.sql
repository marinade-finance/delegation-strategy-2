ALTER TABLE validators
  ADD COLUMN client_id    TEXT     NULL,
  ADD COLUMN client_type  TEXT     NULL,
  ADD COLUMN feature_set  BIGINT   NULL,
  ADD COLUMN shred_version INTEGER NULL,
  ADD COLUMN gossip_port  INTEGER  NULL,
  ADD COLUMN rpc_public   BOOLEAN  NULL,
  ADD COLUMN pubsub_public BOOLEAN NULL;

ALTER TABLE versions
  ADD COLUMN client_id    TEXT     NULL,
  ADD COLUMN client_type  TEXT     NULL,
  ADD COLUMN feature_set  BIGINT   NULL,
  ADD COLUMN shred_version INTEGER NULL;
