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

## releases

Release metadata for the `releases` table: when a version was published, and the two floors it had
to clear. Mainnet only. One fetcher per source, selected with `--source`:

| Source | Fetcher | Gives |
|---|---|---|
| `github` | GitHub releases for `anza-xyz/agave` and `firedancer-io/firedancer` | publish timestamps, full history, no floors |
| `sfdp` | `api.solana.org/api/community/v1/sfdp_required_versions`, one request per epoch | the Solana Foundation Delegation Program floor, per epoch, from epoch 688 on |
| `feature-gates` | Anza's feature gate tracker wiki plus the gates' own accounts on chain | the floor the cluster enforces, from the epoch each gate activated |

```bash
export RPC_URL=...

# Steady state: the recent floor window plus the current release lists.
cargo run --bin collect -- releases > releases.yaml

# One-off historical backfill of every floor the endpoint answers for.
cargo run --bin collect -- releases --from-epoch 688 > releases.yaml
```

Adding a source means implementing `ReleaseFetcher` and adding a `--source` value; the table does
not change. Rows from different sources coexist and are resolved by the precedence documented in
`migrations/0027-releases.sql`.

The SFDP endpoint answers one epoch per request and starts refusing a few hundred requests in, so
the fetcher paces itself: a full backfill from epoch 688 takes about four minutes. Epochs it has no
answer for reply 404, which is recorded as "no floor stated", not as a failure. `GITHUB_TOKEN`
raises the GitHub rate limit but is not needed for a single run.

`available_epoch` is not stored at all: `collect` emits `released_at` and the read resolves the
epoch against the `epochs` table.

The feature-gate floor is the running maximum, over the gates activated so far, of the version each
gate shipped in. The tracker JSON gives the version, the gate's account gives the activation epoch.

Only gates requiring at least the floor `version-floor.json` publishes are read: that floor is the
maximum over everything already activated, so anything below it is history the migration seeds.