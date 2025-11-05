#!/usr/bin/env sh
set -e

# Get list of changed Rust files (relative to repo root)
CHANGED_RS_FILES=$(git diff --cached --name-only --diff-filter=ACM | grep '\.rs$' || true)

if [ -z "$CHANGED_RS_FILES" ]; then
  echo "No Rust files changed, skipping clippy"
  exit 0
fi

# Get the absolute path of the workspace root
WORKSPACE_ROOT=$(cargo metadata --no-deps --format-version 1 | jq -r '.workspace_root')

# For each package, check if any changed file is in its directory
cargo metadata --no-deps --format-version 1 | \
  jq -r '.packages[] | "\(.name)|\(.manifest_path)"' | \
  while IFS='|' read -r name manifest; do
    pkg_dir=$(dirname "$manifest")
    pkg_rel_dir=${pkg_dir#$WORKSPACE_ROOT/}

    # If pkg_rel_dir is empty, package is at root - match any file
    if [ -z "$pkg_rel_dir" ] || [ "$pkg_rel_dir" = "$pkg_dir" ]; then
      # Root package - any changed Rust file affects it
      if [ -n "$CHANGED_RS_FILES" ]; then
        echo "Running clippy on: $name"
        cargo clippy -p "$name"
      fi
    else
      # Check if any changed file starts with this package's directory
      if echo "$CHANGED_RS_FILES" | grep -q "^$pkg_rel_dir/"; then
        echo "Running clippy on: $name"
        cargo clippy -p "$name"
      fi
    fi
  done
