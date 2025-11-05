# API

Exposing delegation strategy as `validators-api`.

## Development

See how it is configured to be run from code in [ops-infra repository](https://github.com/marinade-finance/ops-infra/blob/master/argocd/delegation-strategy/overlays/prod/kustomization.yaml). 

See [DEVELOPMENT.md](../DEVELOPMENT.md) for local PostgreSQL setup.

```bash
export RPC_URL=...
export POSTGRES_URL='postgresql://delegation-strategy:delegation-strategy@localhost:5432/delegation-strategy'
export PG_SSLROOTCERT='/tmp/postgres-root-cert.pem'

cargo run --bin api -- \
  --postgres-ssl-root-cert $PG_SSLROOTCERT --postgres-url $POSTGRES_URL \
  --scoring-url https://scoring.marinade.finance --admin-auth-token ABCD \
  --blacklist-path ./blacklist.csv --glossary-path ./glossary.md
```

```bash
curl 'http://localhost:8000/validators'
```

**NOTE:**
  To display any data, it must already be stored in the PostgreSQL database
  by the [store process](../store). All subcommand data needs to be stored first.
  Additionally, if there isn’t enough historical data,
  the [join SQL query in store](../store/src/utils.rs) will not filter
  the results properly.
  In that case, the [`list_validators`](./src/handlers/list_validators.rs)
  function must be modified to return the data directly without filtering, i.e.:
  ```rust
  return Ok(validators.into_values().collect());
  ```
