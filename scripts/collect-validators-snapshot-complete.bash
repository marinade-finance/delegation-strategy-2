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

EPOCH=$(( $(solana epoch) - 1 ))

"$BIN_DIR/collect" \
  --url "$RPC_URL" \
  validators \
    --epoch "$EPOCH" \
    --with-rewards \
    --with-validator-info \
    --whois "https://whois.marinade.finance" \
    --whois-bearer-token "$WHOIS_BEARER_TOKEN" \
    --escrow-relocker "tovt1VkTE2T4caWoeFP6a2xSFoew5mNpd7FWidyyMuk" \
    --gauge-meister "mvgmBamY7hDWxLNGLshMoZn8nt2P8tKnKhaBeXMVajZ"
