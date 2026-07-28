# Development notes

Running the collectors CLI locally.

# 1. Run PosgreSQL and apply migrations scripts

```bash
export DB='delegation-strategy'

docker run --name postgresql-${DB} -p 5432:5432 --rm \
  -e POSTGRES_DB=${DB} \
  -e POSTGRES_USER=${DB} \
  -e POSTGRES_PASSWORD=${DB} \
  postgres:17.4 \
  -c max-prepared-transactions=100 \
  -c log-statement=all \
  -c ssl=on \
  -c ssl_cert_file=/etc/ssl/certs/ssl-cert-snakeoil.pem \
  -c ssl_key_file=/etc/ssl/private/ssl-cert-snakeoil.key

export DB='delegation-strategy'
for FILE in ./migrations/*.sql; do
  echo "Migration SQL init execution: $FILE"
  PGPASSWORD=${DB} psql -U ${DB} -d ${DB} \
    -h localhost -p 5432 -f "$FILE"
done

docker cp postgresql-${DB}:/etc/ssl/certs/ssl-cert-snakeoil.pem /tmp/postgres-root-cert.pem
```

The PostgreSQL URL is then `postgresql://delegation-strategy:delegation-strategy@localhost:5432/delegation-strategy`

# 2. Run the SQL loader tests

`store/tests/cluster_stats_sql.rs` exercises the `/cluster-stats` and
`/validators/flat` queries against a real PostgreSQL. It creates its own schema,
applies `migrations/*.sql` into it and drops it again, so it needs an empty
database only on the first run.

```bash
export DS_TEST_POSTGRES_URL='postgresql://delegation-strategy:delegation-strategy@localhost:5432/delegation-strategy'
cargo test --all-features
```

Without `DS_TEST_POSTGRES_URL` the tests report why they are skipped and pass.
On Podman, start the container with `podman run` and export
`DOCKER_HOST=unix:///run/user/1000/podman/podman.sock`.
