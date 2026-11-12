// Byte-identical copy in ds-sam packages/ds-sam-calc/test/fixtures; a rule change updates both.
use serde::Deserialize;
use store::incidents::{sandwich_hit, sandwich_population};

#[derive(Deserialize)]
struct Cases {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    floor_pct: f64,
    rows: Vec<Row>,
    expected: Expected,
}

#[derive(Deserialize)]
struct Row {
    vote_account: String,
    blocks_produced: u64,
    blocks_with_sandwiches: u64,
    sandwich_rate_30d: f64,
}

#[derive(Deserialize)]
struct Expected {
    median: Option<f64>,
    pooled: Option<f64>,
    hits: Vec<Hit>,
}

#[derive(Deserialize)]
struct Hit {
    vote_account: String,
    p_value: f64,
}

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() <= 1e-12 * actual.abs().max(expected.abs())
}

#[test]
fn the_incidents_sandwich_test_matches_the_shared_cases() {
    let cases: Cases = serde_json::from_str(include_str!("fixtures/sandwich-parity.json")).unwrap();
    assert!(cases.cases.len() >= 8);

    for case in cases.cases {
        let population = sandwich_population(case.rows.iter().map(|r| {
            (
                r.sandwich_rate_30d,
                r.blocks_produced,
                r.blocks_with_sandwiches,
            )
        }));
        assert_eq!(
            population.map(|p| p.median_rate),
            case.expected.median,
            "{}: median",
            case.name
        );
        match (population, case.expected.pooled) {
            (Some(p), Some(pooled)) => {
                assert!(close(p.pooled_rate, pooled), "{}: pooled", case.name)
            }
            (None, None) => {}
            _ => panic!("{}: population", case.name),
        }

        let hits: Vec<(String, f64)> = population
            .map(|population| {
                case.rows
                    .iter()
                    .filter_map(|r| {
                        sandwich_hit(
                            r.sandwich_rate_30d,
                            r.blocks_produced,
                            r.blocks_with_sandwiches,
                            &population,
                            case.floor_pct,
                        )
                        .map(|p| (r.vote_account.clone(), p))
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            hits.iter().map(|(a, _)| a.as_str()).collect::<Vec<_>>(),
            case.expected
                .hits
                .iter()
                .map(|h| h.vote_account.as_str())
                .collect::<Vec<_>>(),
            "{}: hits",
            case.name
        );
        for ((account, actual), expected) in hits.iter().zip(&case.expected.hits) {
            assert!(
                close(*actual, expected.p_value),
                "{}: {account} p-value {actual} != {}",
                case.name,
                expected.p_value
            );
        }
    }
}
