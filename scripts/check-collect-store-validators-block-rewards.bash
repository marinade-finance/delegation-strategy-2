#!/bin/bash

SCRIPT_DIR=$(dirname "$0")

collect_store_validators_block_rewards() {
  local output_file="validators-block-rewards.yaml"
  echo "Checking validators block rewards..."

  ${SCRIPT_DIR}/check-validators-block-rewards.bash
  local exit_code=$?
  case $exit_code in
    0)
      set -e
      echo "Collecting validators block rewards"
      ${SCRIPT_DIR}/collect-validators-block-rewards.bash > "$output_file"
      ${SCRIPT_DIR}/store-validators-block-rewards.bash "$output_file"
      set +e
      ;;
    1)
      echo "Not a good time to collect validators block rewards"
      return 0
      ;;
    *)
      echo "An unexpected error occurred on checking validators block rewards. Exit code: $exit_code"
      exit $exit_code
      ;;
  esac
}

collect_store_validators_block_rewards
