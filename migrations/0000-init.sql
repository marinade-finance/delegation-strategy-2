CREATE TABLE uptimes (
  id BIGSERIAL NOT NULL,
  identity TEXT NOT NULL,
  status TEXT CHECK (status IN ('UP', 'DOWN')) NOT NULL,
  start_at TIMESTAMP WITH TIME ZONE NOT NULL,
  end_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(id)
);
CREATE INDEX uptime_identity_end_at ON uptimes (identity, end_at);
CREATE INDEX uptime_identity ON uptimes (identity);

CREATE TABLE commissions (
  id BIGSERIAL NOT NULL,
  identity TEXT NOT NULL,
  commission INTEGER NOT NULL,
  epoch_slot INTEGER NOT NULL,
  epoch INTEGER NOT NULL,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(id)
);
CREATE INDEX commission_changes_identity_created_at ON commissions (identity, created_at);

CREATE TABLE versions (
  id BIGSERIAL NOT NULL,
  identity TEXT NOT NULL,
  version TEXT,
  epoch_slot INTEGER NOT NULL,
  epoch INTEGER NOT NULL,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(id)
);
CREATE INDEX version_changes_identity_created_at ON versions (identity, created_at);

CREATE TABLE validators (
  identity TEXT NOT NULL,
  vote_account TEXT NOT NULL,
  epoch INTEGER NOT NULL,

  PRIMARY KEY(id)
);

CREATE TABLE cluster_info (

);

CREATE TABLE epochs (

);
