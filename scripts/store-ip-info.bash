#!/bin/bash

set -e

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"

if [[ -z $POSTGRES_URL ]]
then
  echo "Env variable POSTGRES_URL is missing!"
  exit 1
fi

if [[ -z $WHOIS_BEARER_TOKEN ]]
then
  echo "Env variable WHOIS_BEARER_TOKEN is missing!" >&2
  exit 1
fi

"$BIN_DIR/store" \
  --postgres-url "$POSTGRES_URL" \
  ip-info \
    --whois "https://whois.marinade.finance" \
    --whois-bearer-token "$WHOIS_BEARER_TOKEN" \
    "$@"
