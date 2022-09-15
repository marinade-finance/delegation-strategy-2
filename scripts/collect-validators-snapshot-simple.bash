#!/bin/bash

SCRIPT_DIR=$(dirname "$0")
BIN_DIR=$SCRIPT_DIR/../target/debug

"$BIN_DIR/collect" \
  --url http://api.mainnet-beta.solana.com \
  validators
