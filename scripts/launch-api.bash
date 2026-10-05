#!/bin/bash

set -e

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"
GLOSSARY_MD="${GLOSSARY_MD:-"$SCRIPT_DIR/../glossary.md"}"
BLACKLIST_CSV="${BLACKLIST_CSV:-"$SCRIPT_DIR/../blacklist.cache.csv"}"

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

"$BIN_DIR/api" \
  --directory-url "$DIRECTORY_URL" \
  --directory-token "$DIRECTORY_TOKEN" \
  --glossary-path "$GLOSSARY_MD" \
  --blacklist-path "$BLACKLIST_CSV"
