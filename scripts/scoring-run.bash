#!/bin/bash

set -uxo

script_dir=$(dirname "$0")
working_directory=${SCORING_WORKING_DIRECTORY:-"$script_dir/.."}
file_scoring_r=${SCORING_R:-"$script_dir/scoring.R"}

touch "$working_directory/scores.csv"
touch "$working_directory/stakes.csv"
touch "$working_directory/unstakes.txt"

Rscript --vanilla "$file_scoring_r" \
    "$(realpath "$working_directory/scores.csv")" \
    "$(realpath "$working_directory/stakes.csv")" \
    "$(realpath "$working_directory/unstakes.txt")" \
    "$(realpath "$working_directory/params.env")" \
    "$(realpath "$working_directory/blacklist.csv")" \
    "$(realpath "$working_directory/validators.csv")" \
    "$(realpath "$working_directory/self-stake.csv")"
