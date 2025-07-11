#!/bin/bash

set -e

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"

SUBCOMMAND="$1"
if [[ "$SUBCOMMAND" != "jito-mev" && "$SUBCOMMAND" != "jito-priority" ]]; then
  echo "Usage: $0 <jito-mev|jito-priority>" >&2
  exit 21
fi

if [[ -z $RPC_URL ]]; then
  echo "Env variable RPC_URL is missing!" >&2
  exit 22
fi

"$BIN_DIR/collect" \
  --url "$RPC_URL" \
  $SUBCOMMAND
