# store CLI

Storing YAML data files collected by [collect process](../collect) into PostgreSQL database.

## Development

For local development check to run the PostgreSQL database ad [DEVELOPMENT.md](../DEVELOPMENT.md).

```bash
export RPC_URL=...
export POSTGRES_URL='postgresql://delegation-strategy:delegation-strategy@localhost:5432/delegation-strategy'

cargo run --bin store -- \
  --postgres-ssl-root-cert /tmp/postgres-root-cert.pem --postgres-url $POSTGRES_URL
  <<SUBCOMMAND>> --snapshot-file <<FILE-PATH>>
```

For example:

```bash
export POSTGRES_URL='postgresql://delegation-strategy:delegation-strategy@localhost:5432/delegation-strategy'
export PG_SSLROOTCERT='/tmp/postgres-root-cert.pem'

cargo run --bin store -- --postgres-url $POSTGRES_URL \
  cluster-info --snapshot-file /tmp/snapshot-performance.yaml

cargo run --bin store -- --postgres-url $POSTGRES_URL \
  close-epoch --snapshot-file /tmp/snapshot-performance-last-epoch.yaml

cargo run --bin store -- --postgres-url $POSTGRES_URL \
  jito-priority --snapshot-file /tmp/jito-priority.yaml
  
cargo run --bin store -- --postgres-url $POSTGRES_URL \
  jito-mev --snapshot-file /tmp/jito-mev.yaml

cargo run --bin store -- --postgres-url $POSTGRES_URL \
  validators --snapshot-file /tmp/validators.yaml
```
