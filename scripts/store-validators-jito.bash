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

if [[ -z $POSTGRES_URL ]]; then
  echo "Env variable POSTGRES_URL is missing!" >&2
  exit 23
fi
if [[ -z $PG_SSLROOTCERT ]]; then
  echo "Env variable PG_SSLROOTCERT is missing!" >&2
  exit 24
fi

SNAPSHOT="$1"
if [[ -z $SNAPSHOT ]]; then
  echo "$USAGE" >&2
  exit 25
fi

"$BIN_DIR/store" \
  --postgres-url "$POSTGRES_URL" \
  $SUBCOMMAND \
    --snapshot-file "$SNAPSHOT"
