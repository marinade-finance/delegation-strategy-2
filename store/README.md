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
cargo run --bin store -- \
  --postgres-ssl-root-cert /tmp/postgres-root-cert.pem --postgres-url $POSTGRES_URL \
  cluster-info --snapshot-file /tmp/snapshot-performance.yaml

cargo run --bin store -- \
  --postgres-ssl-root-cert /tmp/postgres-root-cert.pem --postgres-url $POSTGRES_URL \
  jito-priority --snapshot-file /tmp/jito-priority.yaml
```
