#!/bin/bash

set -e

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"

if [[ -z $POSTGRES_URL ]]
then
  echo "Env variable POSTGRES_URL is missing!"
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
  versions \
    --snapshot-file "$SNAPSHOT"
