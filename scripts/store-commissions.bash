#!/bin/bash

SCRIPT_DIR=$(dirname "$0")
BIN_DIR=$SCRIPT_DIR/../target/debug

if [[ -z $POSTGRES_URL ]]
then
  echo "Env variable POSTGRES_URL is missing!"
  exit 1
fi

"$BIN_DIR/store" \
  --postgres-url "$POSTGRES_URL" \
  --snapshot-file "$SCRIPT_DIR/../snapshot.yaml" \
  commissions
