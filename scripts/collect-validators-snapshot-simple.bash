#!/bin/bash

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="$SCRIPT_DIR/../target/release"

if [[ -z $RPC_URL ]]
then
  echo "Env variable RPC_URL is missing!" >&2
  exit 1
fi

"$BIN_DIR/collect" \
  --url "$RPC_URL" \
  validators
