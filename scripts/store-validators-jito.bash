#!/bin/bash

set -e

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"

case "$1" in
    mev) SUBCOMMAND="jito-mev" ;;
    priority-fee) SUBCOMMAND="jito-priority" ;;
    *) echo "Usage: $0 <mev|priority-fee>" >&2; exit 1 ;;
esac
shift

if [[ -z $POSTGRES_URL ]]
then
  echo "Env variable POSTGRES_URL is missing!" >&2
  exit 1
fi

SNAPSHOT="$1"
if [[ -z $SNAPSHOT ]]
then
  echo "Usage: $0 <snapshot-file>" >&2
  exit 1
fi

"$BIN_DIR/store" \
  --postgres-url "$POSTGRES_URL" \
  $SUBCOMMAND \
    --snapshot-file "$SNAPSHOT"
