#!/bin/bash

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"

if [[ -z $RPC_URL ]]
then
  echo "Env variable RPC_URL is missing!" >&2
  exit 1
fi

EPOCH=$(( $(solana -u "$RPC_URL" epoch) - 1 ))

"$BIN_DIR/collect" \
  --url "$RPC_URL" \
  validators-performance \
    --with-rewards \
    --epoch "$EPOCH"
