#!/bin/bash

set -e

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"
USAGE="Usage: $0 <jito-mev|jito-priority> <path-to-snapshot-file>"

SUBCOMMAND="$1"
if [[ "$SUBCOMMAND" != "jito-mev" && "$SUBCOMMAND" != "jito-priority" ]]; then
  echo "$USAGE" >&2
  exit 21
fi
shift

if [[ -z $DIRECTORY_URL ]]; then
  echo "Env variable DIRECTORY_URL is missing!" >&2
  exit 23
fi
if [[ -z $DIRECTORY_TOKEN ]]; then
  echo "Env variable DIRECTORY_TOKEN is missing!" >&2
  exit 24
fi

SNAPSHOT="$1"
if [[ -z $SNAPSHOT ]]; then
  echo "$USAGE" >&2
  exit 25
fi

"$BIN_DIR/store" \
  --directory-url "$DIRECTORY_URL" \
  --directory-token "$DIRECTORY_TOKEN" \
  $SUBCOMMAND \
    --snapshot-file "$SNAPSHOT"
