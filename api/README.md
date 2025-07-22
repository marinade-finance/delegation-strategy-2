# API

Exposing delegation strategy as `validators-api`.

## Development

See how it is configured to be run from code in [ops-infra repository](https://github.com/marinade-finance/ops-infra/blob/master/argocd/delegation-strategy/overlays/prod/kustomization.yaml). 

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
