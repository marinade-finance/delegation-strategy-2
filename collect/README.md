# collect CLI

Collecting on-chain data to YAML files.

## Development

```bash
export RPC_URL=...

cargo run --bin collect -- validators-performance | tee /tmp/snapshot-performance.yaml
cargo run --bin collect -- jito-priority | tee /tmp/jito-priority.yaml
```
