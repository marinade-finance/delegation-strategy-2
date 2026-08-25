-- Enrichment keyed by the address it describes, not by the validator sitting on it: the address is
-- what moves, so one row per IP joined against the observation log gives location history for free.
CREATE TABLE ip_info (
  ip TEXT NOT NULL,
  asn BIGINT NULL,
  aso TEXT NULL,
  continent TEXT NULL,
  country_iso TEXT NULL,
  country TEXT NULL,
  city TEXT NULL,
  coordinates_lat DOUBLE PRECISION NULL,
  coordinates_lon DOUBLE PRECISION NULL,
  fetched_at TIMESTAMP WITH TIME ZONE NOT NULL,

  PRIMARY KEY(ip)
);

CREATE INDEX idx_ip_info_fetched_at ON ip_info(fetched_at);
