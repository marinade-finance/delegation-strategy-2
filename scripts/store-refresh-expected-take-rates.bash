#!/bin/bash

set -e

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"

if [[ -z $POSTGRES_URL ]]
then
  echo "Env variable POSTGRES_URL is missing!" >&2
  exit 1
fi

FROM_EPOCH="$1"
if [[ -z $FROM_EPOCH ]]
then
  echo "Usage: $0 <from-epoch>" >&2
  exit 1
fi

"$BIN_DIR/store" \
  --postgres-url "$POSTGRES_URL" \
  refresh-expected-take-rates \
    --from-epoch "$FROM_EPOCH"
