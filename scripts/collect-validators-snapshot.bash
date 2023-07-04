#!/bin/bash

set -e

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"

if [[ -z $WHOIS_BEARER_TOKEN ]]
then
  echo "Env variable WHOIS_BEARER_TOKEN is missing!" >&2
  exit 1
fi

if [[ -z $RPC_URL ]]
then
  echo "Env variable RPC_URL is missing!" >&2
  exit 1
fi

"$BIN_DIR/collect" \
  --url "$RPC_URL" \
  validators \
    --whois "https://whois.marinade.finance" \
    --whois-bearer-token "$WHOIS_BEARER_TOKEN" \
    --vemnde-votes-json "./mnde_votes_snapshot.json"