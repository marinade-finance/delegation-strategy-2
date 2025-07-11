# check CLI

Verification if it is time to save the data to the database.

## Development

For local development check to run the PostgreSQL database ad [DEVELOPMENT.md](../DEVELOPMENT.md).

```
export RPC_URL=...
export POSTGRES_URL='postgresql://delegation-strategy:delegation-strategy@localhost:5432/delegation-strategy'

cargo run --bin check -- --postgres-url "$POSTGES_URL" jito-priority
```
