#!/bin/bash

set -e

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"

if [[ -z $DIRECTORY_URL ]]
then
  echo "Env variable DIRECTORY_URL is missing!" >&2
  exit 1
fi

if [[ -z $DIRECTORY_TOKEN ]]
then
  echo "Env variable DIRECTORY_TOKEN is missing!" >&2
  exit 1
fi

SNAPSHOT="$1"
if [[ -z $SNAPSHOT ]]
then
  echo "Usage: $0 <snapshot-file>" >&2
  exit 1
fi

"$BIN_DIR/store" \
  --directory-url "$DIRECTORY_URL" \
  --directory-token "$DIRECTORY_TOKEN" \
  close-epoch \
    --snapshot-file "$SNAPSHOT"
