#!/bin/bash

set -uxo

script_dir=$(dirname "$0")
working_directory=${SCORING_WORKING_DIRECTORY:-"$script_dir/.."}
file_scoring_r=${SCORING_R:-"$script_dir/scoring.R"}
file_scoring_rmd=${SCORING_RMD:-"$script_dir/scoring.Rmd"}

touch "$working_directory/report.html"

# Rscript --vanilla "$file_scoring_r" ./scores.csv ./stakes.csv ./params.env ./blacklist.csv ./validators.csv ./self-stake.csv
Rscript -e "rmarkdown::render('$file_scoring_rmd', output_file = '$(realpath "$working_directory/report.html")')" \
    "$(realpath "$working_directory/scores.csv")" \
    "$(realpath "$working_directory/stakes.csv")" \
    "$(realpath "$working_directory/params.env")" \
    "$(realpath "$working_directory/blacklist.csv")" \
    "$(realpath "$working_directory/validators.csv")" \
    "$(realpath "$working_directory/self-stake.csv")"