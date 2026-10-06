# store CLI

Storing YAML data files collected by [collect process](../collect) as documents
in [marinade-directory](../DEVELOPMENT.md).

## Development

See [DEVELOPMENT.md](../DEVELOPMENT.md) for the local store setup, which also
exports `DIRECTORY_URL` and `DIRECTORY_TOKEN`.

```bash
cargo run --bin store -- \
  --directory-url "$DIRECTORY_URL" --directory-token "$DIRECTORY_TOKEN" \
  <<SUBCOMMAND>> --snapshot-file <<FILE-PATH>>
```

Example:

```bash
OUTPUT_DIR=/tmp/collect-output
mkdir -p $OUTPUT_DIR
STORE="cargo run --bin store -- --directory-url $DIRECTORY_URL --directory-token $DIRECTORY_TOKEN"

$STORE validators --snapshot-file "$OUTPUT_DIR"/validators.yaml

# store-cluster-info
$STORE cluster-info --snapshot-file "$OUTPUT_DIR"/snapshot-performance.yaml
# store-quick-changes
$STORE uptime --snapshot-file "$OUTPUT_DIR"/snapshot-performance.yaml
$STORE versions --snapshot-file "$OUTPUT_DIR"/snapshot-performance.yaml
$STORE commissions --snapshot-file "$OUTPUT_DIR"/snapshot-performance.yaml
# store-epoch-close (writes /validators/epochs/{epoch} last)
$STORE close-epoch --snapshot-file "$OUTPUT_DIR"/snapshot-performance-last-epoch.yaml

$STORE validators-block-rewards --snapshot-file "$OUTPUT_DIR"/validators-block-rewards.yaml
$STORE validators-events --snapshot-file "$OUTPUT_DIR"/validators-events.yaml
$STORE validators-sandwiches --snapshot-file "$OUTPUT_DIR"/validators-sandwiches.yaml
$STORE take-rates --snapshot-file "$OUTPUT_DIR"/take-rates.yaml
$STORE releases --snapshot-file "$OUTPUT_DIR"/releases.yaml

$STORE jito-priority --snapshot-file "$OUTPUT_DIR"/jito-priority.yaml
$STORE jito-mev --snapshot-file "$OUTPUT_DIR"/jito-mev.yaml
```

## Documents

| path | written by | holds |
|---|---|---|
| `/validators/snapshot/{epoch}` | `validators`, `close-epoch` | the per-epoch validator records |
| `/validators/mev/{epoch}` | `jito-mev` | the latest MEV observation per vote account |
| `/validators/priority-fee/{epoch}` | `jito-priority` | the latest priority-fee observation |
| `/validators/events/{epoch}` | `validators-events` | PSR settlements per vote account |
| `/validators/block-rewards/{epoch}` | `validators-block-rewards` | block rewards per identity and vote account |
| `/validators/validator-rewards/{epoch}` | `take-rates` | both sides of every reward component per vote account, and the take rate they make |
| `/validators/sandwiches/{epoch}` | `validators-sandwiches` | the 30-day sandwich figures per vote account |
| `/validators/epochs/{epoch}` | `close-epoch`, last | the sealed signal: start, end, supply, inflation |
| `/validators/live/{uptimes,commissions,versions,cluster-info}` | the minute writers | the accumulators, under compare-and-swap |
| `/validators/{uptimes,commissions,versions,cluster-info}/{epoch}` | `close-epoch` | what the accumulators held when the epoch closed |
| `/validators/releases` | `releases` | every client release by lineage and version: publish time, SFDP floor, feature-gate floor |
