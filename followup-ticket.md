# Check which stake the Alpenglow uptime formula must use

## Why

DS2, ds-sam and ds-scoring compute the Alpenglow uptime with
`uptime = (reward/w) / (Σreward/Σw)` and `w = stake/S + leader_slots/N`.
All three use `activated_stake` of epoch E as `stake`.

Agave can pay the rewards of epoch E on a different stake. The DS2 #298
description says that agave freezes `epoch_stakes(E)` at the start of E-1.
DS2 serves that value as `epoch_stake`, which is `activated_stake` of E-1.

If that is correct, the formula gives a wrong ratio when stake activates or
deactivates at the start of E.

Example: a validator has 100k SOL in E-1 and gets 100k SOL more in E.
Agave pays on 100k SOL, but the formula expects a reward for 200k SOL.
A validator with no downtime then gets a ratio of about 0.67, not 1.
If the stake triples or more, the ratio falls below the 0.5 limit of the
DS2 `LowCredits` hint.

## What

1. Find in the agave source which stake pays the vote rewards of epoch E.
2. Find which stake makes the leader schedule of epoch E.
3. Compare `activated_stake` and `epoch_stake` against the reward of real
   Alpenglow epochs on testnet. Use validators with a large stake change.
4. If `epoch_stake` is the correct input, change DS2, ds-sam and
   ds-scoring together, and update the ts-common test vectors.

## Constraint

All three repos must use the same stake. Do not change one repo alone.

## Reference

- DS2 #298: serve `epoch_stake` on validators `epoch_stats`
- ds-scoring #53, ds-sam #150, typescript-common #130
