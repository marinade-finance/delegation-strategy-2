#!/bin/bash

SCRIPT_DIR=$(dirname "$0")
BIN_DIR="${BIN_DIR:-"$SCRIPT_DIR/../target/debug"}"

"$BIN_DIR/collect" \
  blacklist-auctions "$@"
