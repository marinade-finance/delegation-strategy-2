#!/bin/bash

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"

if [[ -z $RPC_URL ]]
then
  echo "Env variable RPC_URL is missing!" >&2
  exit 1
fi
if [[ -z $GOOGLE_APPLICATION_CREDENTIALS || ! -f "$GOOGLE_APPLICATION_CREDENTIALS" ]]
then
  echo "Env variable GOOGLE_APPLICATION_CREDENTIALS is missing or points to a non-existent file!" >&2
  exit 2
fi

"$BIN_DIR/collect" \
  --url "$RPC_URL" \
  validators-block-rewards
