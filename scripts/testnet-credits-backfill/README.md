# Testnet credits backfill

A one-time fix of the testnet database, in `backfill.sql`.

## What it changes

The SQL changes only the `credits` and `vote_reward_lamports` columns of the
`validators` table, in testnet epochs 1042-1053. It inserts and deletes no rows.

1. In epochs 1042-1053, it moves the value of `credits` to `vote_reward_lamports`
   and sets `credits` to `NULL`. A row with zero credits and zero stake gets
   `NULL` in `vote_reward_lamports`.
2. In epoch 1042, it sets `credits` to the tower vote credits from the on-chain
   `epochCredits` of each vote account.

Epoch 1042 is the Alpenglow migration epoch. In this epoch, a vote account
earned tower vote credits before the migration and reward lamports after it.
The file holds both values for each vote account, read from the testnet RPC.

## Why

Testnet ran Alpenglow from epoch 1042, before the collector supported it.
Before #292, the collector stored the Alpenglow vote reward in lamports in
`validators.credits`. The `vote_reward_lamports` column did not exist.

Hotfix #307 stops the API crash on these values. But the epochs still give
wrong `LowCredits` hints and a wrong `avg_adjusted_vote_performance`.

The normal finalize run cannot fix these epochs. `getVoteAccounts` gives only
the last 5 `epochCredits` entries, so the old epochs are not available there.

## Run

```bash
psql "$POSTGRES_URL" -v ON_ERROR_STOP=1 -f scripts/testnet-credits-backfill/backfill.sql
```

The file runs in one transaction. It stops and changes nothing in these cases:

- One of epochs 1042-1053 has no `credits` above the tower limit. Thus the
  database is not testnet, or the backfill already ran.
- A stored `credits` value of epoch 1042 is not the on-chain reward of that
  vote account.

After the backfill, you can remove the overflow check from hotfix #307.
