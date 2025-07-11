#!/bin/bash

SCRIPT_DIR=$(dirname "$0")

collect_store_jito() {
  local type="$1"
  local output_file="validators-${type}.yaml"
  echo "Checking validators ${type}..."

  ${SCRIPT_DIR}/check-validators-jito.bash "$type"
  local exit_code=$?
  case $exit_code in
    0)
      set -e
      echo "Collecting JITO validators '${type}'"
      ${SCRIPT_DIR}/collect-validators-jito.bash "$type" > "$output_file"
      echo "Storing JITO validators '${type}'"
      ${SCRIPT_DIR}/store-validators-jito.bash "$type" "$output_file"
      set +e
      ;;
    1)
      # Exit code 1: validators MEV already collected
      echo "Not a good time to collect validators '${type}'"
      return 0
      ;;
    *)
      # Any other exit code: throw the error
      echo "An unexpected error occurred on checking '${type}'. Exit code: $exit_code"
      exit $exit_code
      ;;
  esac
}

for type in "jito-priority" "jito-mev"; do
  collect_store_jito "$type"
done
