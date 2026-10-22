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

cargo build --bin store
export POSTGRES_URL="host=$PG_SOCKET_DIR user=${DB} password=${DB} dbname=${DB}"
export PG_SSLROOTCERT=/etc/ssl/certs/ca-certificates.crt
./scripts/migrate.bash
```

`scripts/migrate.bash` runs `store migrate`, which applies each file in `migrations/` and
records it in the `migrations` table. Run it again after you pull new migration files.

Socket connections skip TLS, so `PG_SSLROOTCERT` only needs to name an existing CA file.
Keep the socket directory path short: a Unix socket path is capped at 107 bytes.

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

Add the next file in `migrations/`, for example `0041-what-it-does.sql`. `store migrate`
(`scripts/migrate.bash`) runs each file that the `migrations` table does not list, in number order,
and records its name. All pending files run in one transaction (`store/src/migrations.rs`).
If one file fails, every file of that run rolls back, and the `migrations` table is unchanged.
In Kubernetes, an Argo CD PreSync job runs it before the sync. A failed run stops the sync, and
the old pods stay. The API does not apply migrations.

- Name each file `NNNN-what-it-does.sql`, with the next free 4-digit number.
- Never edit a file after it ran in an environment. Add a new file.
- Never write a top-level `BEGIN`, `COMMIT`, `END`, `ROLLBACK` or `ABORT` in a file.
  A `COMMIT` or `END` fails the run and rolls it back. A `ROLLBACK` or `ABORT` fails the run,
  but the statements after it in that file stay committed. A `BEGIN ... END;` inside a `DO`
  block is allowed.
- Never use `CREATE INDEX CONCURRENTLY` or `VACUUM`. They cannot run in a transaction, so the
  run fails.
- After `ALTER TYPE ... ADD VALUE`, the new value is not usable until the run commits. Do not
  use it in a later file of the same release.
- The run holds all its locks until the last file commits. Keep a long backfill out of a
  release that also locks a table the API reads.
- The schema changes before the new API and collectors start, so the old API and collectors
  run on the new schema for a short time. Make each migration work with the previous release.
