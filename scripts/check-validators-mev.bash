#!/bin/bash

set -e
set -x

pwd
ls -alrth

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"

ls -alrth "$BIN_DIR"

if [[ -z $RPC_URL ]]
then
  echo "Env variable RPC_URL is missing!" >&2
  exit 1
fi

if [[ -z $POSTGRES_URL ]]
then
  echo "Env variable POSTGRES_URL is missing!" >&2
  exit 1
fi

"$BIN_DIR/check" \
  --rpc-url "$RPC_URL" \
  --postgres-url "$POSTGRES_URL" \
  validators-mev
