#!/bin/bash

set -e

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"

case "$1" in
    mev) SUBCOMMAND="jito-mev" ;;
    priority-fee) SUBCOMMAND="jito-priority" ;;
    *) echo "Usage: $0 <mev|priority-fee>" >&2; exit 1 ;;
esac

if [[ -z $RPC_URL ]]
then
  echo "Env variable RPC_URL is missing!" >&2
  exit 1
fi

"$BIN_DIR/collect" \
  --url "$RPC_URL" \
  $SUBCOMMAND
