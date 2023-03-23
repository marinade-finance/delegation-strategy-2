#!/bin/bash

set -exu

file_epoch_info_response="./epoch-info.txt"
file_response_tvl="./tvl.txt"
file_response_self_stake="./self-stake.txt"
file_parsed_self_stake="./self-stake.csv"
file_validators="./validators.csv"
file_blacklist="./blacklist.csv"
file_params="./params.env"
file_unstake_hints="./unstake-hints.json"

current_epoch=$(curl -sfLS http://api.mainnet-beta.solana.com -X POST -H "Content-Type: application/json" -d '
  {"jsonrpc":"2.0","id":1, "method":"getEpochInfo"}
' | jq '.result.epoch' -r)

curl -sfLS http://api.marinade.finance/tlv > "$file_response_tvl"
TOTAL_STAKE=$(<"$file_response_tvl" jq 'fromjson? | .total_virtual_staked_sol' -R)

echo "Total Stake: $TOTAL_STAKE"

curl -sfLS http://stake-monitor.marinade.finance > "$file_response_self_stake"
echo "vote_account,current_balance,deposited_balance" > "$file_parsed_self_stake"
<"$file_response_self_stake" jq 'fromjson? | .[] | [.voteAccount, .total, .depositStakeAmount + .depositSolAmount] | @csv' -R -r >> "$file_parsed_self_stake"

curl -sfLS "https://validators-api-dev.marinade.finance/validators/flat?last_epoch=$(( current_epoch - 1 ))" > "$file_validators"

curl -sfLS "http://localhost:8000/unstake-hints?epoch=412&eepoch=$(( current_epoch ))" | jq > "$file_unstake_hints"

curl -sfLS "https://raw.githubusercontent.com/marinade-finance/delegation-strategy-2/master/blacklist.csv" > "$file_blacklist"

cat <<EOF > "$file_params"
TOTAL_STAKE=$TOTAL_STAKE

MARINADE_VALIDATORS_COUNT=100

WEIGHT_ADJUSTED_CREDITS=10
WEIGHT_GRACE_SKIP_RATE=1
WEIGHT_DC_CONCENTRATION=2

COMPONENTS=COMMISSION_ADJUSTED_CREDITS,GRACE_SKIP_RATE,DC_CONCENTRATION
COMPONENT_WEIGHTS=10,1,2

ELIGIBILITY_ALGO_STAKE_MAX_COMMISSION=10
ELIGIBILITY_ALGO_STAKE_MIN_STAKE=1000

ELIGIBILITY_MNDE_STAKE_MAX_COMMISSION=10
ELIGIBILITY_MNDE_STAKE_MIN_STAKE=100
ELIGIBILITY_MNDE_SCORE_THRESHOLD_MULTIPLIER=0.9

MNDE_VALIDATOR_CAP=0.1

STAKE_CONTROL_MNDE=0.2
STAKE_CONTROL_SELF_STAKE_MAX=0.3
EOF

cat "$file_params"
