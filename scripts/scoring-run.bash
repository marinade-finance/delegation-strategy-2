#!/bin/bash

set -uxo

file_scoring_r=${SCORING_R:-"./scripts/scoring.R"}

Rscript --vanilla "$file_scoring_r" ./score.csv ./params.env ./validators.csv ./self-stake.csv
