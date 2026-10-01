# Testnet: credits after the Alpenglow migration

Testnet moved from TowerBFT to Alpenglow in epoch 1042. After the migration, `epochCredits` in the vote state counts lamports, not vote credits.

## What the vote state shows

The runtime writes a marker entry `(u64::MAX, u64::MAX, u64::MAX)` at the migration boundary. The migration epoch then has two entries: one before the marker and one after it.

Real values for vote account `KmCRTozzcAXvFEH2xakNMV7GeWyftyVFKS32XGV4spW`, read with `getAccountInfo` and `jsonParsed` in epoch 1047:

| Entry | epoch | credits | previousCredits | Delta |
|---|---|---|---|---|
| Tower | 1041 | 810676609 | 803969051 | 6707558 vote credits |
| Tower half | 1042 | 810741247 | 810676609 | 64638 vote credits |
| Marker | u64::MAX | u64::MAX | u64::MAX | |
| Alpenglow half | 1042 | 291696003588 | 810741247 | 290885262341 lamports |
| Alpenglow | 1043 | 590625720152 | 291696003588 | 298929716564 lamports |

- `getVoteAccounts` returns only the last 5 entries of `epochCredits`, and the marker is one of the 5.
- The vote state keeps 64 entries.
- `lastVote` is 0 when the vote state has no votes. SIMD-0357 says that the `votes` list becomes empty under Alpenglow. In epoch 1047, `lastVote` still had values on testnet.

## The problem in DS2 before the fix

- The collector stored the lamport delta in `validators.credits` for every epoch from 1042 on.
- In epoch 1042, the collector kept only the Alpenglow half. The tower half was lost.
- `SUM(credits * activated_stake)` went above the `Decimal` maximum. See commit deadc80.
- If the epoch was not in the 5-entry window, the finalize step stored 0 as `credits`.

## What DS2 stores now

| Column | Tower epoch | Migration epoch | Alpenglow epoch |
|---|---|---|---|
| `credits_regime` | `tower` | `migration` | `alpenglow` |
| `credits` | vote credits | tower half, vote credits | `NULL` |
| `alpenglow_credits` | `NULL` | Alpenglow half, lamports | lamports |
| `epoch_credits_raw` | the `getVoteAccounts` window | the window | the window |

- `epoch_credits_raw` is JSON text. Use `epoch_credits_raw::jsonb` to query it.
- If the epoch is not in the window, the store keeps the values that it has.
- `uptimes.last_credits` holds the latest cumulative credits of the vote account.
- If `lastVote` is 0, a validator is DOWN when its credits did not increase since the previous snapshot. Otherwise, DS2 uses the `delinquent` list from `getVoteAccounts`.

## Rows that stay wrong

We did no backfill. Testnet rows from epoch 1042 until the deploy epoch hold lamports in `credits`. Their `credits_regime` is `NULL`.

Use this query to find them:

```sql
SELECT epoch, COUNT(*) AS rows, MAX(credits) AS max_credits
FROM validators
WHERE epoch >= 1042 AND credits_regime IS NULL
GROUP BY epoch
ORDER BY epoch;
```

A tower epoch has at most 432000 slots and at most 16 credits for each slot. A value of `credits` above 6912000 is not a vote credit count.

## Effect on consumers

From the deploy epoch on, `credits` is `NULL` for each Alpenglow epoch on testnet. The consumers do not use `alpenglow_credits` yet.

- APY: `apr` and `apy` are `null`, because `total_weighted_credits` has no value for the epoch.
- `/validators/flat`: `HAVING COUNT(*) FILTER (WHERE credits > 0) >= 7` removes a validator when fewer than 7 epochs in the window have credits.
- Eligibility: a validator is eligible only if it has stake, because `credits` counts as 0.
- `LowCredits` unstake hint: the store gives no hint for a `NULL` value.
- Scoring: `avg_adjusted_credits` uses only the tower epochs in the window. It is 0 when the window has no tower epoch.

## Run the collector on testnet

The collector finds the migration epoch from the marker. An active vote account loses the marker from its `getVoteAccounts` window 4 epochs after the migration. In epoch 1047, only 11 inactive vote accounts still had the marker in the window. If none has it, the collector stores lamports as tower credits and logs a warning. Set the migration epoch on testnet:

```bash
export ALPENGLOW_MIGRATION_EPOCH=1042
collect validators-performance --with-rewards --epoch "$EPOCH"
```

The `validators` command reads the same variable. Do not set it on mainnet until mainnet migrates.
