#!/bin/bash

set -e

SCRIPT_DIR=$(dirname "$0")

"$SCRIPT_DIR/store-uptimes.bash" $@
"$SCRIPT_DIR/store-commissions.bash" $@
"$SCRIPT_DIR/store-versions.bash" $@
# Last of the four: set -e aborts the rest of the chain on failure, and this is the writer with no consumers yet.
"$SCRIPT_DIR/store-node-observations.bash" $@
