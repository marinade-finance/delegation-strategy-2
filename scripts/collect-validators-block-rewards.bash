#!/bin/bash

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"

if [[ -z $RPC_URL ]]
then
  echo "Env variable RPC_URL is missing!" >&2
  exit 1
fi
if [[ -n "$GOOGLE_APPLICATION_CREDENTIALS" ]] || [[ -n "$GOOGLE_APPLICATION_CREDENTIALS_JSON" ]]; then
  : # At least one valid file exists, continue
else
  echo "Neither 'GOOGLE_APPLICATION_CREDENTIALS' nor 'GOOGLE_APPLICATION_CREDENTIALS_JSON' envs are defined" >&2
  exit 2
fi

"$BIN_DIR/collect" \
  --url "$RPC_URL" \
  validators-block-rewards
