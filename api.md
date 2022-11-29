# API docs
## List validators
Query parameters:
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
      "dc_ip": "3.144.92.238",
      "dc_coordinates_lat": 39.9625,
      "dc_coordinates_lon": -83.0061,
      "dc_continent": "NA",
      "dc_country_iso": "US",
      "dc_country": "United States",
      "dc_city": "Columbus",
      "dc_asn": 16509,
      "dc_aso": "AMAZON-02",
      "commission_max_observed": null,
      "commission_min_observed": null,
      "commission_advertised": 8,
      "commission_effective": null,
      "version": "1.13.5",
      "mnde_votes": "0",
      "activated_stake": "8946164901061930",
      "marinade_stake": "0",
      "decentralizer_stake": "0",
      "superminority": false,
      "credits": 130511,
      "marinade_score": 0,
      "epoch_stats": [
        {
          "epoch": 379,
          "commission_max_observed": null,
          "commission_min_observed": null,
          "commission_advertised": 8,
          "commission_effective": null,
          "version": "1.13.5",
          "mnde_votes": 0,
          "activated_stake": 8946164901061930,
          "marinade_stake": 0,
          "decentralizer_stake": 0,
          "superminority": false,
          "stake_to_become_superminority": 0,
          "credits": 130511,
          "leader_slots": 3412,
          "blocks_produced": 3360,
          "skip_rate": 0.015240328253223967,
          "uptime_pct": null,
          "uptime": null,
          "downtime": null
        },
        {
          "epoch": 378,
          "commission_max_observed": null,
          "commission_min_observed": null,
          "commission_advertised": 8,
          "commission_effective": 8,
          "version": "1.13.5",
          "mnde_votes": 0,
          "activated_stake": 8939087071594018,
          "marinade_stake": 0,
          "decentralizer_stake": 0,
          "superminority": false,
          "stake_to_become_superminority": 0,
          "credits": 403929,
          "leader_slots": 10980,
          "blocks_produced": 10874,
          "skip_rate": 0.0096539162112933,
          "uptime_pct": null,
          "uptime": null,
          "downtime": null
        }
      ]
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
curl -sfLS localhost:8000/glossary
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
      "change": 50000000000000,
      "identity": "XkCriyrNwS3G4rzAXtG5B1nnvb5Ka1JtCku93VqeKAr",
      "stake": 1000000000000000
    },
    {
      "change": -50000000000000,
      "identity": "Awes4Tr6TX8JDzEhCZY2QVNimT6iD1zWHzf1vNyGvpLM",
      "stake": 50000000000000
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
