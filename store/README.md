# store CLI

Storing YAML data files collected by [collect process](../collect) into PostgreSQL database.

## Development

See [DEVELOPMENT.md](../DEVELOPMENT.md) for local PostgreSQL setup.

```bash
export DB='delegation-strategy'
export POSTGRES_URL="postgresql://${DB}:${DB}@localhost:5432/${DB}"

cargo run --bin store -- \
  --postgres-ssl-root-cert /tmp/postgres-root-cert.pem --postgres-url $POSTGRES_URL
  <<SUBCOMMAND>> --snapshot-file <<FILE-PATH>>
```

Example:

```bash
export DB='delegation-strategy'
export POSTGRES_URL="postgresql://${DB}:${DB}@localhost:5432/${DB}"
export PG_SSLROOTCERT='/tmp/postgres-root-cert.pem'

OUTPUT_DIR=/tmp/collect-output
mkdir -p $OUTPUT_DIR

cargo run --bin store -- --postgres-url $POSTGRES_URL \
  validators --snapshot-file "$OUTPUT_DIR"/validators.yaml

# store-cluster-info
cargo run --bin store -- --postgres-url $POSTGRES_URL \
  cluster-info --snapshot-file "$OUTPUT_DIR"/snapshot-performance.yaml
# store-quick-changes
cargo run --bin store -- --postgres-url $POSTGRES_URL \
  uptime --snapshot-file "$OUTPUT_DIR"/snapshot-performance.yaml
cargo run --bin store -- --postgres-url $POSTGRES_URL \
  versions --snapshot-file "$OUTPUT_DIR"/snapshot-performance.yaml
cargo run --bin store -- --postgres-url $POSTGRES_URL \
  commissions --snapshot-file "$OUTPUT_DIR"/snapshot-performance.yaml
# store-epoch-close (table: epochs)
cargo run --bin store -- --postgres-url $POSTGRES_URL \
  close-epoch --snapshot-file "$OUTPUT_DIR"/snapshot-performance-last-epoch.yaml

cargo run --bin store -- --postgres-url $POSTGRES_URL \
  validators-block-rewards --snapshot-file "$OUTPUT_DIR"/validators-block-rewards.yaml


cargo run --bin store -- --postgres-url $POSTGRES_URL \
  jito-priority --snapshot-file "$OUTPUT_DIR"/jito-priority.yaml
cargo run --bin store -- --postgres-url $POSTGRES_URL \
  jito-mev --snapshot-file "$OUTPUT_DIR"/jito-mev.yaml
```
