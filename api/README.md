# API

Exposing delegation strategy as `validators-api`.

## Development

See how it is configured to be run from code in [ops-infra repository](https://github.com/marinade-finance/ops-infra/blob/master/argocd/delegation-strategy/overlays/prod/kustomization.yaml). 

See [DEVELOPMENT.md](../DEVELOPMENT.md) for the local store setup, which also
exports `DIRECTORY_URL` and `DIRECTORY_TOKEN`.

```bash
export RPC_URL=...

cargo run --bin api -- \
  --directory-url "$DIRECTORY_URL" --directory-token "$DIRECTORY_TOKEN" \
  --admin-auth-token ABCD \
  --blacklist-path ./blacklist.csv --glossary-path ./glossary.md
```

```bash
curl 'http://localhost:8000/validators'

EPOCH=$(( $(solana -um epoch) - 1 ))
curl "http://localhost:8000/unstake-hints?epoch=$EPOCH"
```

**NOTE:**
  To display any data, it must already be stored as documents
  by the [store process](../store). All subcommand data needs to be stored first.
  Additionally, if there isn’t enough historical data,
  the [folds in store](../store/src/utils.rs) will not filter
  the results properly.
  In that case, the [`list_validators`](./src/handlers/list_validators.rs)
  function must be modified to return the data directly without filtering, i.e.:
  ```rust
  return Ok(validators.into_values().collect());
  ```
