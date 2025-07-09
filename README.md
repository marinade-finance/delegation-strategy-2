# Delegation Strategy

This repository contains all necessary components to track and collect validators' data and evaluate them for the purposes of Marinade's stake distribution.

## Repository structure
- [Rust API to expose all validators' data](./api)
- [Rust CLI tool to collect a snapshot from the Solana chain](./collect)
- [Rust CLI tool to store previously collected data to DB](./store)
- [DB migration scripts](./migrations)
- [A set of scoring and utility scripts](./scripts)

## Scoring
### Replicate official reports
Follow the steps described for each report on [the staking reports page](https://marinade.finance/validators/reports/)

### Create a new report
Prerequisities:
- [R installed](https://cran.r-project.org/)
- R packages installed (run from the cloned repository):
```bash
./scripts/scoring-install.bash
```

Inside the cloned repository run:
```bash
./scripts/scoring-fetch-inputs.bash                   # To fetch the latest data from API
./scripts/scoring-run.bash                            # To generate scores
./scripts/scoring-report.bash "- development version" # To generate report.html
```

## Development
### Prerequisities
- Rust - for development of data collection, storing and serving
- PostgreSQL - for the storage
- R - for the scoring calculations

### Env
Create a `.env` file after cloning the repository
```envc
POSTGRES_URL=postgresql://...
RPC_URL=https://api.mainnet-beta.solana.com
WHOIS_BEARER_TOKEN=...
ADMIN_AUTH_TOKEN=...
```

### Build
```bash
cargo build
```

## Automation
Automated pipelines that take care of running the scoring and storing the reports are located in the [Delegation Strategy - Pipeline](https://github.com/marinade-finance/delegation-strategy-pipeline) repository.

## Architecture
```mermaid
C4Context
    title Delegation Strategy

    Enterprise_Boundary(E_SOLANA, "Solana") {
        Person(VALIDATOR, "Validator")
        SystemDb_Ext(CHAIN, "Cluster")
    }
    Enterprise_Boundary(E_GEODB, "Maxmind") {
        SystemDb_Ext(GEO_DB, "Maxmind Geo DB")
    }
    Enterprise_Boundary(E_MARINADE, "Marinade") {
        System(DS_COLLECTOR, "Delegation Strategy Collector", "Collects data about validators")
        SystemDb(DS_DB, "Delegation Strategy DB", "Stores all collected data")
        System(DS_API, "Delegation Strategy Public API", "Provides access to collected data")
        System(DS_PIPELINE, "Delegation Strategy Pipeline", "Provides access to collected data")
        SystemDb(DS_GITHUB_PIPELINE, "Delegation Strategy Pipeline Repository", "Stores all historical scoring inputs/outputs.")
    }
    Enterprise_Boundary(E_PUBLIC, "General Public") {
        Person(DS_USER, "External User", "A user who want to use our data.")
    }

    BiRel(VALIDATOR, CHAIN, "")
    Rel(CHAIN, DS_COLLECTOR, "Cluster and validators' data")
    UpdateRelStyle(CHAIN, DS_COLLECTOR, $textColor="yellow", $lineColor="yellow", $offsetY="-10")
    Rel(GEO_DB, DS_COLLECTOR, "IP address data")
    UpdateRelStyle(GEO_DB, DS_COLLECTOR, $textColor="yellow", $lineColor="yellow")

    Rel(DS_COLLECTOR, DS_DB, "Aggregated data")
    UpdateRelStyle(DS_COLLECTOR, DS_DB, $textColor="yellow", $lineColor="yellow")
    Rel(DS_DB, DS_API, "Aggregated data")
    UpdateRelStyle(DS_DB, DS_API, $textColor="yellow", $lineColor="yellow")

    Rel(DS_API, DS_USER, "Collected and processed data")
    UpdateRelStyle(DS_API, DS_USER, $textColor="yellow", $lineColor="yellow")
    Rel(DS_GITHUB_PIPELINE, DS_USER, "Scoring results")
    UpdateRelStyle(DS_GITHUB_PIPELINE, DS_USER, $textColor="yellow", $lineColor="yellow")

    Rel(DS_API, DS_PIPELINE, "Collected and processed data")
    UpdateRelStyle(DS_API, DS_PIPELINE, $textColor="yellow", $lineColor="yellow")

    Rel(DS_PIPELINE, DS_GITHUB_PIPELINE, "Scoring results")
    UpdateRelStyle(DS_PIPELINE, DS_GITHUB_PIPELINE, $textColor="yellow", $lineColor="yellow")

    UpdateLayoutConfig($c4ShapeInRow="3", $c4BoundaryInRow="4")
```
