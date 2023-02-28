#!/bin/bash

file_scoring_r=${SCORING_R:-"./scripts/scoring.R"}

Rscript "$file_scoring_r" ./score.csv ./params.env ./validators.csv ./self-stake.csv
