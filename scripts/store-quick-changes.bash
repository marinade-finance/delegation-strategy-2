#!/bin/bash

set -e

SCRIPT_DIR=$(dirname "$0")

"$SCRIPT_DIR/store-uptimes.bash" $@
"$SCRIPT_DIR/store-commissions.bash" $@
"$SCRIPT_DIR/store-versions.bash" $@
