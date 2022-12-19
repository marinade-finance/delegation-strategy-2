# API docs
## List validators
Query parameters:
- `epochs` - Optional, limits history size.
- `query` - Optional, filters results based on `identity`, `vote_address`, `info_name`.
- `query_identities` - Optional, comma separated list of identities to fetch.
- `order_field` - Default `Stake`, possible values: `Stake`, `Credits`, `MndeVotes`.
- `order_direction` - Default `DESC`, possible values: `ASC`, `DESC`.
- `offset` - Default `0`.
- `limit` - Default `100`.
```bash
curl -sfLS 'localhost:8000/validators?limit=1&offset=0' | jq
```
```json
{
  "validators": [
    {
      "identity": "XkCriyrNwS3G4rzAXtG5B1nnvb5Ka1JtCku93VqeKAr",
      "vote_account": "beefKGBWeSpHzYBHZXwp5So7wdQGX6mu4ZHCsH3uTar",
      "info_name": "Coinbase Cloud",
      "info_url": "https://www.coinbase.com/cloud",
      "info_keybase": "coinbasecloud",
      "node_ip": "3.144.92.238",
      "dc_coordinates_lat": 39.9625,
      "dc_coordinates_lon": -83.0061,
      "dc_continent": "NA",
      "dc_country_iso": "US",
      "dc_country": "United States",
      "dc_city": "Columbus",
      "dc_full_city": "NA/United States/Columbus",
      "dc_asn": 16509,
      "dc_aso": "AMAZON-02",
      "dcc_full_city": 0.04088592163799646,
      "dcc_asn": 0.12072577643919202,
      "dcc_aso": 0.12072577643919202,
      "commission_max_observed": null,
      "commission_min_observed": null,
      "commission_advertised": 8,
      "commission_effective": null,
      "commission_aggregated": null,
      "version": "1.13.5",
      "mnde_votes": "0",
      "activated_stake": "9488392700088216",
      "marinade_stake": "0",
      "decentralizer_stake": "0",
      "superminority": true,
      "credits": 156411,
      "marinade_score": 0,
      "warnings": [],
      "epoch_stats": [
        {
          "epoch": 388,
          "commission_max_observed": null,
          "commission_min_observed": null,
          "commission_advertised": 8,
          "commission_effective": null,
          "version": "1.13.5",
          "mnde_votes": 0,
          "activated_stake": 9488392700088216,
          "marinade_stake": 0,
          "decentralizer_stake": 0,
          "superminority": true,
          "stake_to_become_superminority": 0,
          "credits": 156411,
          "leader_slots": 4436,
          "blocks_produced": 4314,
          "skip_rate": 0.027502254283138017,
          "uptime_pct": null,
          "uptime": null,
          "downtime": null,
          "apr": null,
          "apy": null,
          "marinade_score": 0,
          "rank_marinade_score": 2371,
          "rank_activated_stake": 1,
          "rank_apy": null
        }
      ],
      "epochs_count": 7,
      "avg_uptime_pct": 0.9971032356617813,
      "avg_apy": 0.08364469455818813
    }
  ]
}
```

## Uptimes
```bash
curl -sfLS localhost:8000/validators/XkCriyrNwS3G4rzAXtG5B1nnvb5Ka1JtCku93VqeKAr/uptimes | jq
```
```json
{
  "uptimes": [
    {
      "epoch": 378,
      "status": "UP",
      "start_at": "2022-11-27T19:45:05.669098Z",
      "end_at": "2022-11-28T13:46:02.217154Z"
    },
    {
      "epoch": 378,
      "status": "DOWN",
      "start_at": "2022-11-28T13:46:02.217154Z",
      "end_at": "2022-11-28T13:47:02.217154Z"
    }
  ]
}
```

## Commissions
```bash
curl -sfLS localhost:8000/validators/XkCriyrNwS3G4rzAXtG5B1nnvb5Ka1JtCku93VqeKAr/commissions | jq
```
```json
{
  "commissions": [
    {
      "epoch": 378,
      "commission": 8,
      "created_at": "2022-11-28T15:58:04.038843Z"
    }
  ]
}
```

## Versions
```bash
curl -sfLS localhost:8000/validators/XkCriyrNwS3G4rzAXtG5B1nnvb5Ka1JtCku93VqeKAr/versions | jq
```
```json
{
  "versions": [
    {
      "epoch": 378,
      "version": "1.13.5",
      "created_at": "2022-11-28T15:58:04.038843Z"
    }
  ]
}
```

## Glossary
```bash
curl -sfLS localhost:8000/static/glossary.md
# Glossary

...
```

## Reports - staking
```bash
curl -sLfS 'http://localhost:8000/reports/staking' | jq
```
```json
{
  "planned": [
    {
      "current_stake": 1000000000000000,
      "identity": "XkCriyrNwS3G4rzAXtG5B1nnvb5Ka1JtCku93VqeKAr",
      "immediate": true,
      "next_stake": 1200000000000000
    },
    {
      "current_stake": 50000000000000,
      "identity": "Awes4Tr6TX8JDzEhCZY2QVNimT6iD1zWHzf1vNyGvpLM",
      "immediate": true,
      "next_stake": 0
    },
    {
      "current_stake": 20000000000000,
      "identity": "DRpbCBMxVnDK7maPM5tGv6MvB3v1sRMC86PZ8okm21hy",
      "immediate": false,
      "next_stake": 0
    }
  ]
}
```

## Reports - scoring
```bash
curl -sLfS 'http://localhost:8000/reports/scoring' | jq
```
```json
{
  "reports": {
    "370": [
      {
        "created_at": "2022-11-29T17:20:01.123456Z",
        "link": "https://..../....zip",
        "md": "Download data for the report: ```bash\nsh -c \"echo Downloaded...\"```\nGenerate the report: ```bash\nsh -c \"echo Generated...\"```\n"
      },
      {
        "created_at": "2022-11-28T13:46:02.217154Z",
        "link": "https://..../....zip",
        "md": "Download data for the report: ```bash\nsh -c \"echo Downloaded...\"```\nGenerate the report: ```bash\nsh -c \"echo Generated...\"```\n"
      }
    ]
  }
}
```

## Reports - commission changes
```bash
curl -sfLS localhost:8000/reports/commission-changes | jq
```
```json
{
  [
    {
      "identity": "8xuQB5uNAEAxPz1tTeGc9zU6FVLWiB2WySTL8ZbkydsV",
      "from": 10,
      "to": 100,
      "epoch": 382,
      "epoch_slot": 55450
    },
    {
      "identity": "EeWuLmFPuEbeAmyNAtQQSLsYJ9ppjLGkGgGYFm2S4WDg",
      "from": 10,
      "to": 100,
      "epoch": 382,
      "epoch_slot": 55450
    }
  ]
}
```

## Config
```bash
curl -sfLS localhost:8000/static/config | jq
```
```json
{
  "stakes": {
    "delegation_authorities": [
      {
        "delegation_authority": "4bZ6o3eUUNXhKuqjdCnCoPAoLgWiuLYixKaxoa8PpiKk",
        "name": "Marinade"
      },
      {
        "delegation_authority": "noMa7dN4cHQLV4ZonXrC29HTKFpxrpFbDLK5Gub8W8t",
        "name": "Marinade's Decentralizer"
      }
    ]
  }
}
```

## Cluster stats
```bash
curl -sfLS 'localhost:8000/cluster-stats?epochs=1' | jq
```
```json
{
  "cluster_stats": {
    "block_production_stats": [
      {
        "epoch": 388,
        "blocks_produced": 168499,
        "leader_slots": 175025,
        "avg_skip_rate": 0.037286101985430656
      }
    ],
    "dc_concentration_stats": [
      {
        "epoch": 388,
        "total_activated_stake": 0,
        "dc_concentration_by_aso": {
          "GOOGLE-CLOUD-PLATFORM": 4.2850976516686704e-05,
          ...
        },
        "dc_stake_by_aso": {
          "GOOGLE-CLOUD-PLATFORM": 15763676302719,
          ...
        },
        "dc_concentration_by_asn": {
          "396982": 4.2850976516686704e-05,
          ...
        },
        "dc_stake_by_asn": {
          "396982": 15763676302719,
          ...
        },
        "dc_concentration_by_city": {
          "EU/Ukraine/Kyiv": 0.0001018310885876089,
          ...
        },
        "dc_stake_by_city": {
          "EU/Ukraine/Kyiv": 37460810663754,
          ...
        }
      }
    ]
  }
}
```

## Metrics
```bash
curl -sLfS 'http://localhost:9000/metrics'
```
