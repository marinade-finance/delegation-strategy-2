#!/bin/bash

R --vanilla -e 'install.packages("dotenv", repos = "http://cran.us.r-project.org")'
R --vanilla -e 'install.packages("data.table", repos = "http://cran.us.r-project.org")'
R --vanilla -e 'install.packages("rmarkdown", repos = "http://cran.us.r-project.org")'
R --vanilla -e 'install.packages("treemapify", repos = "http://cran.us.r-project.org")'
R --vanilla -e 'install.packages("gridExtra", repos = "http://cran.us.r-project.org")'
