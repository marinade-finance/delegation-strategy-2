# check CLI

Verification if it is time to save the data to the database.

## Development

See [DEVELOPMENT.md](../DEVELOPMENT.md) for local PostgreSQL setup.

```
export RPC_URL=...
export POSTGRES_URL='postgresql://delegation-strategy:delegation-strategy@localhost:5432/delegation-strategy'

cargo run --bin check -- --postgres-url "$POSTGRES_URL" jito-priority
```
