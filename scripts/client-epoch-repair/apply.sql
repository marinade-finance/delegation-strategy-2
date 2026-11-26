-- psql -v epoch=1063 [-v dry_run=1] -f apply.sql
\set ON_ERROR_STOP on
BEGIN;

-- Rows with a NULL client_id_raw carry no client, so their time counts for no candidate.
CREATE TEMP VIEW client_fill AS
WITH ep AS (
  SELECT end_at FROM epochs WHERE epoch = :epoch
), spans AS (
  SELECT
    v.vote_account,
    v.client_id,
    v.client_id_raw,
    v.version,
    GREATEST(COALESCE(LEAD(v.created_at) OVER (PARTITION BY v.vote_account ORDER BY v.created_at, v.id), ep.end_at) - v.created_at, INTERVAL '0') AS held
  FROM versions v, ep
  WHERE v.epoch = :epoch
), totals AS (
  SELECT
    vote_account,
    client_id,
    client_id_raw,
    version,
    SUM(held) AS held,
    SUM(SUM(held)) OVER (PARTITION BY vote_account, COALESCE(client_id::TEXT, client_id_raw)) AS client_held,
    SUM(SUM(held)) OVER (PARTITION BY vote_account, COALESCE(client_id::TEXT, client_id_raw), version) AS version_held
  FROM spans
  WHERE client_id_raw IS NOT NULL
  GROUP BY 1, 2, 3, 4
), ranked AS (
  SELECT
    vote_account,
    client_id,
    client_id_raw,
    version,
    ROW_NUMBER() OVER (PARTITION BY vote_account ORDER BY client_held DESC, version_held DESC, held DESC, client_id_raw, version) AS rn
  FROM totals
)
SELECT
  val.vote_account,
  val.epoch,
  val.activated_stake,
  val.client_id_raw AS old_client_id_raw,
  split_part(val.version, '.', 1) AS old_major,
  r.client_id,
  r.client_id_raw,
  COALESCE(r.version, val.version) AS version
FROM validators val
JOIN ranked r ON r.vote_account = val.vote_account AND r.rn = 1
WHERE val.epoch = :epoch
  AND (val.client_id IS DISTINCT FROM r.client_id
    OR val.client_id_raw IS DISTINCT FROM r.client_id_raw
    OR val.version IS DISTINCT FROM COALESCE(r.version, val.version));

SELECT
  old_client_id_raw,
  old_major,
  client_id_raw AS new_client_id_raw,
  split_part(version, '.', 1) AS new_major,
  COUNT(*) AS validators,
  ROUND(SUM(activated_stake) / 1e15, 2) AS msol
FROM client_fill
GROUP BY 1, 2, 3, 4
ORDER BY msol DESC;

UPDATE validators v
SET
  client_id = f.client_id,
  client_id_raw = f.client_id_raw,
  version = f.version,
  client_id_source = 'versions'
FROM client_fill f
WHERE v.vote_account = f.vote_account
  AND v.epoch = f.epoch;

\if :{?dry_run}
SELECT
  client_id_raw,
  split_part(version, '.', 1) AS major,
  COUNT(*) AS validators,
  ROUND(SUM(activated_stake) / 1e15, 2) AS msol
FROM validators
WHERE epoch = :epoch AND activated_stake > 0
GROUP BY 1, 2
ORDER BY msol DESC;
ROLLBACK;
\else
COMMIT;
\endif
