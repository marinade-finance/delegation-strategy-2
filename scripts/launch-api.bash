#!/bin/bash

set -e

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"
GLOSSARY_MD="${GLOSSARY_MD:-"$SCRIPT_DIR/../glossary.md"}"
# The api now fetches the blacklist from --blacklist-url into this cache path.
BLACKLIST_CSV="${BLACKLIST_CSV:-"$SCRIPT_DIR/../blacklist.cache.csv"}"

if [[ -z $POSTGRES_URL ]]
then
  echo "Env variable POSTGRES_URL is missing!" >&2
  exit 1
fi

"$BIN_DIR/api" \
  --postgres-url "$POSTGRES_URL" \
  --glossary-path "$GLOSSARY_MD" \
  --blacklist-path "$BLACKLIST_CSV" \
  --scoring-url "$SCORING_URL"
