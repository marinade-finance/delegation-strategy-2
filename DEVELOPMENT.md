# Development notes

Running the collectors CLI locally.

# 1. Run PosgreSQL and apply migrations scripts

The container gets no network (`--network none`) and serves only a Unix socket in a host
directory, so it adds no bridge, veth or iptables rules and nothing on the LAN can reach it.

```bash
export DB='delegation-strategy'
export PG_SOCKET_DIR=/tmp/ds-pg
mkdir -m 777 -p "$PG_SOCKET_DIR"

docker run -d --name postgresql-${DB} --rm --network none \
  -v "$PG_SOCKET_DIR":/var/run/postgresql \
  -e POSTGRES_DB=${DB} \
  -e POSTGRES_USER=${DB} \
  -e POSTGRES_PASSWORD=${DB} \
  postgres:17.4

for FILE in ./migrations/*.sql; do
  echo "Migration SQL init execution: $FILE"
  docker exec -i postgresql-${DB} psql -v ON_ERROR_STOP=1 -U ${DB} -d ${DB} < "$FILE"
done
```

`scripts/migrate.bash` applies the same files and records them, so it also works against an empty database.

The PostgreSQL URL is then `host=/tmp/ds-pg user=delegation-strategy password=delegation-strategy dbname=delegation-strategy`.
Socket connections skip TLS, so `PG_SSLROOTCERT` only needs to name an existing CA file,
e.g. `/etc/ssl/certs/ca-certificates.crt`. Keep the socket directory path short: a Unix socket
path is capped at 107 bytes.

# 2. Run the SQL loader tests

`store/tests/cluster_stats_sql.rs` exercises the `/cluster-stats` and
`/validators/flat` queries against a real PostgreSQL. It creates its own schema,
applies `migrations/*.sql` into it and drops it again, so it needs an empty
database only on the first run.

```bash
export DS_TEST_POSTGRES_URL='host=/tmp/ds-pg user=delegation-strategy password=delegation-strategy dbname=delegation-strategy'
cargo test --all-features
```

Without `DS_TEST_POSTGRES_URL` the tests report why they are skipped and pass.
On Podman, start the container with `podman run` and export
`DOCKER_HOST=unix:///run/user/1000/podman/podman.sock`.

# 3. Add a migration

Add the next file in `migrations/`, for example `0040-what-it-does.sql`. `store migrate`
(`scripts/migrate.bash`) runs each file that the `migrations` table does not list, in number order,
and records its name. Each file runs in one transaction, and the first error stops the run.
In Kubernetes, an Argo CD PreSync job runs it before the sync. A failed run stops the sync, and
the old pods stay. The API does not apply migrations.

- Name each file `<number>-<name>.sql`, with a number of 4 digits, for example `0040-what-it-does.sql`
- Never edit a file after it ran in production. Add a new file.
- Do not write `BEGIN`, `COMMIT` or `ROLLBACK` in a file. `CREATE INDEX CONCURRENTLY` cannot run
  in a transaction, so it cannot be a migration.
- The schema changes before the new API and collectors start, so the old API and collectors
  run on the new schema for a short time. Make each migration work with the previous release.
