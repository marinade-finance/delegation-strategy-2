# collect CLI

Collecting on-chain data to YAML files.

## Development

See [DEVELOPMENT.md](../DEVELOPMENT.md) for local PostgreSQL setup.

```bash
export RPC_URL=...

EPOCH=$(( $(solana -u "$RPC_URL" epoch) - 1 ))

cargo run --bin collect -- validators-performance --epoch $EPOCH | \
 tee /tmp/snapshot-performance.yaml
cargo run --bin collect -- validators-performance --with-rewards --epoch $EPOCH | \
 tee /tmp/snapshot-performance-last-epoch.yaml

cargo run --bin collect -- jito-priority | tee /tmp/jito-priority.yaml
cargo run --bin collect -- -u $RPC_URL jito-mev | tee /tmp/jito-mev.yaml

cargo run --bin collect -- -u $RPC_URL validators --epoch $EPOCH | tee /tmp/validators.yaml
```
