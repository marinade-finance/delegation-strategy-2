#!/bin/bash

SCRIPT_DIR=$(dirname "$0")

collect_store_take_rates() {
  local output_file="take-rates.yaml"

  set -e
  echo "Collecting take rates"
  # Pass-through args (e.g. --from-epoch N for a historical backfill, or --epochs-back N).
  ${SCRIPT_DIR}/collect-take-rates.bash "$@" > "$output_file"
  if [[ -s "$output_file" ]]; then
    echo "Storing take rates from $output_file"
    ${SCRIPT_DIR}/store-take-rates.bash "$output_file"
  else
    echo "No take rates data collected yet (BigQuery has no data for the window); skipping store, will retry next run"
  fi
  set +e
}

collect_store_take_rates "$@"
