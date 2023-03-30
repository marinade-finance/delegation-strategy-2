#!/bin/bash

R --vanilla -e 'install.packages(c(
        "dotenv",
        "data.table",
        "rmarkdown",
        "dplyr",
        "treemapify",
        "gridExtra",
        "semver"
    ), repos = "http://cran.us.r-project.org")'
