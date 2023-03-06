#!/bin/bash

set -uxo

file_scoring_r=${SCORING_R:-"./scripts/scoring.R"}

Rscript --vanilla "$file_scoring_r" ./scores.csv ./stakes.csv ./params.env ./blacklist.csv ./validators.csv ./self-stake.csv
