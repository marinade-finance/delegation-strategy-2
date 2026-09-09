#!/bin/bash

set -e

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"
GLOSSARY_MD="${GLOSSARY_MD:-"$SCRIPT_DIR/../glossary.md"}"
BLACKLIST_CSV="${BLACKLIST_CSV:-"$SCRIPT_DIR/../blacklist.csv"}"
PROVIDERS_CONFIG_YAML="${PROVIDERS_CONFIG_YAML:-"$SCRIPT_DIR/../providers-config.yaml"}"

if [[ -z $POSTGRES_URL ]]
then
  echo "Env variable POSTGRES_URL is missing!" >&2
  exit 1
fi

"$BIN_DIR/api" \
  --postgres-url "$POSTGRES_URL" \
  --glossary-path "$GLOSSARY_MD" \
  --blacklist-path "$BLACKLIST_CSV" \
  --providers-config-path "$PROVIDERS_CONFIG_YAML" \
  --scoring-url "$SCORING_URL"
