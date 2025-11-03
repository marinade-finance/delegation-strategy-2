# collect CLI

Collecting on-chain data to YAML files.

## Development

See [DEVELOPMENT.md](../DEVELOPMENT.md) for local PostgreSQL setup.

> **NOTE:** we can collect the data from this or previous epochs.
> The reason is that RPC methods normally is not supporting historical data collection.
> Data for testing can be copied from (prod) DB.

```bash
export RPC_URL=...

EPOCH=$(( $(solana -u "$RPC_URL" epoch) - 1 ))

OUTPUT_DIR=/tmp/collect-output
mkdir -p $OUTPUT_DIR

cargo run --bin collect -- validators --epoch $EPOCH | \
  tee "$OUTPUT_DIR"/validators.yaml
cargo run --bin collect -- validators-performance --epoch $EPOCH | \
  tee "$OUTPUT_DIR"/snapshot-performance.yaml

cargo run --bin collect -- jito-priority --current-epoch-override $EPOCH | \
  tee "$OUTPUT_DIR"/jito-priority.yaml
cargo run --bin collect -- jito-mev --current-epoch-override $EPOCH | \
  tee "$OUTPUT_DIR"/jito-mev.yaml

# getBlockProduction method works only with recent epochs (cannot work with EPOCH-1)
cargo run --bin collect -- validators-performance --epoch $EPOCH | \
  tee "$OUTPUT_DIR"/snapshot-performance-last-epoch.yaml
```

To work with Google BigQuery collected data `GOOGLE_APPLICATION_CREDENTIALS` is required.

```bash
export RPC_URL=...
export GOOGLE_APPLICATION_CREDENTIALS=...

EPOCH=$(( $(solana -u "$RPC_URL" epoch) - 1 ))

OUTPUT_DIR=/tmp/collect-output
mkdir -p $OUTPUT_DIR

cargo run --bin collect -- -u $RPC_URL validators-block-rewards --epoch $EPOCH |\
  tee "$OUTPUT_DIR"/validators-block-rewards.yaml
```
