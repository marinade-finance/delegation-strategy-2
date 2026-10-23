# Testnet credits backfill

A record of a one-time fix of the testnet database. The backfill ran on
2026-10-23. Do not run it again.

## Files

| File | Content |
|------|---------|
| `credits-backfill.yaml` | The input of the backfill: the tower credits and the vote reward lamports of each vote account in epochs 1042-1053, read from the testnet RPC. |
| `credits-backup.csv` | The `vote_account`, `epoch`, `credits` and `vote_reward_lamports` columns of the `validators` table in epochs 1042-1053, before the backfill. |

## Why

Testnet ran Alpenglow from epoch 1042, before the collector supported it.
Before #292, the collector stored the Alpenglow vote reward in lamports in
`validators.credits`. The `vote_reward_lamports` column did not exist.

Hotfix #307 stops the API crash on these values. But the epochs still gave
wrong `LowCredits` hints and a wrong `avg_adjusted_vote_performance`.

The normal finalize run cannot fix these epochs. `getVoteAccounts` gives only
the last 5 `epochCredits` entries, so the old epochs are not available there.

## What it changed

The backfill changed only the `credits` and `vote_reward_lamports` columns of
the `validators` table, in testnet epochs 1042-1053. It inserted and deleted no
rows. All changes ran in one transaction.

1. Each row with a vote account in the YAML file got the `credits` and
   `vote_reward_lamports` values from the file. A row with zero stake and a
   zero reward got `NULL` in `vote_reward_lamports`.
2. Each row with a vote account not in the file got its `credits` value moved
   to `vote_reward_lamports`, and `NULL` in `credits`.

Epoch 1042 is the Alpenglow migration epoch. In this epoch, a vote account
earned tower vote credits before the migration and reward lamports after it.
The YAML file holds both values for each vote account.

## How it ran

The code ran at commit `f712679` of the `fix/testnet-credits-backfill` branch.
The branch does not keep the `credits-backfill` subcommands.

```bash
export RPC_URL=<testnet rpc>
cargo run --release --bin collect -- credits-backfill --from-epoch 1042 --to-epoch 1053 > /tmp/credits-backfill.yaml
psql "$POSTGRES_URL" -c "\copy (SELECT vote_account, epoch, credits, vote_reward_lamports FROM validators WHERE epoch BETWEEN 1042 AND 1053) TO '/tmp/credits-backup.csv' CSV HEADER"
cargo run --release --bin store -- --postgres-url "$POSTGRES_URL" --postgres-ssl-root-cert "$PG_SSLROOTCERT" credits-backfill --snapshot-file /tmp/credits-backfill.yaml
```

## Restore

To restore the old values, load `credits-backup.csv` into a table and update
`validators` from it on `vote_account` and `epoch`.
