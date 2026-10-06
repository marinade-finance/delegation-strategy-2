#!/bin/bash

SCRIPT_DIR=$(dirname "$0")

collect_store_releases() {
  local output_file="releases.yaml"

  set -e
  echo "Collecting releases"
  # Pass-through args (e.g. --from-epoch 688 for the full floor history, or --source github).
  ${SCRIPT_DIR}/collect-releases.bash "$@" > "$output_file"
  if [[ -s "$output_file" ]]; then
    echo "Storing releases from $output_file"
    ${SCRIPT_DIR}/store-releases.bash "$output_file"
  else
    echo "No release data collected (upstream unreachable); skipping store, will retry next run"
  fi
  set +e
}

collect_store_releases "$@"
