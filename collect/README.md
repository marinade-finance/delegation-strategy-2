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

cargo run --bin collect -- jito-priority --epoch $EPOCH | \
  tee "$OUTPUT_DIR"/jito-priority.yaml
cargo run --bin collect -- jito-mev --epoch $EPOCH | \
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

## validators-events range

`validators-events` (PSR settlements) is queried by epoch range, upserted idempotently:

- **Recurring cron:** use `--epochs-back N` (bounded window, re-queries the last N epochs each run to backfill late-arriving settlements). This is the steady-state mode. Settlements for epoch X are only generated after X closes (in X+1) and their amounts keep changing over a ~3–4 epoch claim window, so a single latest-epoch query would miss/undercount them — the window re-captures them. See https://docs.marinade.finance/marinade-protocol/protocol-overview/protected-staking-rewards
- **One-off historical backfill:** use `--from-epoch N` (queries all epochs `>= N`).

Runs are stateless (no synced-epoch cursor): each run re-queries its whole window and upserts, so an interrupted run is fixed by simply re-running. Prefer `--epochs-back` for the cron — a fixed `--from-epoch` grows the re-queried window unbounded as epochs advance.
