# check CLI

Verification if it is time to save the data to the store.

## Development

See [DEVELOPMENT.md](../DEVELOPMENT.md) for the local store setup, which also
exports `DIRECTORY_URL` and `DIRECTORY_TOKEN`.

```
export RPC_URL=...
CHECK="cargo run --bin check -- --directory-url $DIRECTORY_URL --directory-token $DIRECTORY_TOKEN"

# verification of /validators/priority-fee
$CHECK jito-priority

# verification of /validators/mev
$CHECK jito-mev

# verification of /validators/block-rewards
$CHECK block-rewards
```
