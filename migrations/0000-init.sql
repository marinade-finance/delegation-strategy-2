CREATE TABLE uptimes (
  id BIGSERIAL NOT NULL,
  identity TEXT NOT NULL,
  status TEXT CHECK (status IN ('UP', 'DOWN')) NOT NULL,
  epoch NUMERIC NOT NULL,
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
  epoch_slot NUMERIC NOT NULL,
  epoch NUMERIC NOT NULL,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(id)
);
CREATE INDEX commission_changes_identity_created_at ON commissions (identity, created_at);

CREATE TABLE versions (
  id BIGSERIAL NOT NULL,
  identity TEXT NOT NULL,
  version TEXT,
  epoch_slot NUMERIC NOT NULL,
  epoch NUMERIC NOT NULL,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(id)
);
CREATE INDEX version_changes_identity_created_at ON versions (identity, created_at);

CREATE TABLE validators (
  identity TEXT NOT NULL,
  vote_account TEXT NOT NULL,
  epoch NUMERIC NOT NULL,
  info_name TEXT NULL,
  info_url TEXT NULL,
  info_keybase TEXT NULL,
  node_ip TEXT NULL,
  dc_coordinates_lat DOUBLE PRECISION NULL,
  dc_coordinates_lon DOUBLE PRECISION NULL,
  dc_continent TEXT NULL,
  dc_country_iso TEXT NULL,
  dc_country TEXT NULL,
  dc_city TEXT NULL,
  dc_asn INTEGER NULL,
  dc_aso TEXT NULL,
  commission_max_observed INTEGER NULL,
  commission_min_observed INTEGER NULL,
  commission_advertised INTEGER NULL,
  commission_effective INTEGER NULL,
  version TEXT NULL,
  mnde_votes NUMERIC NULL,
  activated_stake NUMERIC NOT NULL,
  marinade_stake NUMERIC NOT NULL,
  decentralizer_stake NUMERIC NOT NULL,
  superminority BOOLEAN NOT NULL,
  stake_to_become_superminority NUMERIC NOT NULL,
  credits NUMERIC NOT NULL,
  leader_slots NUMERIC NOT NULL,
  blocks_produced NUMERIC NOT NULL,
  skip_rate DOUBLE PRECISION NOT NULL,
  uptime_pct DOUBLE PRECISION NULL,
  uptime NUMERIC NULL,
  downtime NUMERIC NULL,
  updated_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(identity, epoch)
);

CREATE TABLE cluster_info (
  id BIGSERIAL NOT NULL,
  epoch_slot NUMERIC NOT NULL,
  epoch NUMERIC NOT NULL,
  transaction_count NUMERIC NOT NULL,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(id)
);

CREATE TABLE epochs (
  epoch NUMERIC NOT NULL,
  start_at TIMESTAMP WITH TIME ZONE NOT NULL,
  end_at TIMESTAMP WITH TIME ZONE NOT NULL,
  transaction_count NUMERIC NOT NULL,
  supply NUMERIC NOT NULL,
  inflation DOUBLE PRECISION NOT NULL,
  inflation_taper DOUBLE PRECISION NOT NULL,

  PRIMARY KEY(epoch)
);

CREATE TABLE warnings (
  id BIGSERIAL NOT NULL,
  identity TEXT NOT NULL,
  code TEXT NOT NULL,
  message TEXT NOT NULL,
  details TEXT,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(id)
);
CREATE INDEX warnings_identity ON warnings (identity);
