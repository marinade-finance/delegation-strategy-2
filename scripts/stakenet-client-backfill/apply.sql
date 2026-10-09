-- gzip -dk stakenet-clients.csv.gz && psql -v csv=stakenet-clients.csv [-v dry_run=1] -f apply.sql
\set ON_ERROR_STOP on
BEGIN;

CREATE TEMP TABLE stakenet_clients (
  vote_account TEXT NOT NULL,
  epoch NUMERIC NOT NULL,
  client_id INTEGER NOT NULL,
  version TEXT NULL,
  PRIMARY KEY (vote_account, epoch)
) ON COMMIT DROP;

-- psql does not interpolate variables inside \copy, only in a variable expanded as a whole line.
\set copy_csv '\\copy stakenet_clients FROM ' :'csv' ' WITH CSV HEADER'
:copy_csv

ANALYZE stakenet_clients;

-- StakeNet keeps the minor as u8, so a Frankendancer 0.x minor (0.1204 -> 0.180) is wrong.
CREATE TEMP VIEW stakenet_fill AS
SELECT
  v.vote_account,
  v.epoch,
  v.activated_stake,
  s.client_id,
  CASE WHEN v.version IS NULL AND s.version NOT LIKE '0.%' THEN s.version END AS version
FROM validators v
JOIN stakenet_clients s ON s.vote_account = v.vote_account AND s.epoch = v.epoch
WHERE v.client_id IS NULL
  AND v.client_id_raw IS NULL;

SELECT
  COUNT(*) AS rows_to_fill,
  COUNT(version) AS versions_to_fill,
  COUNT(DISTINCT epoch) AS epochs,
  MIN(epoch) AS first_epoch,
  MAX(epoch) AS last_epoch
FROM stakenet_fill;

UPDATE validators v
SET
  client_id = f.client_id,
  version = COALESCE(v.version, f.version),
  client_id_source = 'stakenet'
FROM stakenet_fill f
WHERE v.vote_account = f.vote_account
  AND v.epoch = f.epoch;

\if :{?dry_run}
SELECT
  epoch,
  ROUND(100.0 * SUM(activated_stake) FILTER (WHERE client_id IS NULL AND client_id_raw IS NULL) / NULLIF(SUM(activated_stake), 0), 2) AS unknown_stake_pct
FROM validators
WHERE epoch BETWEEN 561 AND 1010
GROUP BY epoch
HAVING SUM(activated_stake) FILTER (WHERE client_id IS NULL AND client_id_raw IS NULL) > 0.06 * SUM(activated_stake)
ORDER BY epoch;

SELECT COUNT(*) AS provenance_violations
FROM validators
WHERE client_id_source = 'stakenet'
  AND (client_id IS NULL OR client_id_raw IS NOT NULL OR epoch NOT BETWEEN 561 AND 1010 OR epoch = 598);
ROLLBACK;
\else
COMMIT;
\endif
