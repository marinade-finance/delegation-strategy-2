use super::{BlacklistRule, Hit, Input, RuleInputs, SandwichRow};
use crate::blacklist::config::{RuleId, SandwichConfig};
use crate::incidents::{sandwich_hit, sandwich_population, SandwichPopulation};
use serde_json::{json, Value};

/// The incidents' sandwich definition with the configured floor, on the single and the wide rate.
pub struct Sandwich {
    rate_pct: f64,
}

impl From<&SandwichConfig> for Sandwich {
    fn from(config: &SandwichConfig) -> Self {
        Sandwich {
            rate_pct: config.rate_pct,
        }
    }
}

/// mev-hub publishes only the wide rate; the count is recovered from it for the significance test.
fn wide_count(wide_rate_pct: f64, blocks_produced: u64) -> u64 {
    (wide_rate_pct * blocks_produced as f64 / 100.0).round() as u64
}

fn observed(
    rate_pct: f64,
    blocks_produced: u64,
    sandwiched: u64,
    population: &SandwichPopulation,
    p_value: f64,
) -> Value {
    json!({
        "rate_30d_pct": rate_pct,
        "blocks_produced": blocks_produced,
        "blocks_with_sandwiches": sandwiched,
        "cluster_median_pct": population.median_rate,
        "cluster_pooled_pct": population.pooled_rate,
        "p_value": p_value,
    })
}

impl BlacklistRule for Sandwich {
    fn id(&self) -> RuleId {
        RuleId::Sandwich
    }

    fn inputs(&self) -> &'static [Input] {
        &[Input::Sandwiches]
    }

    fn evaluate(&self, inputs: &RuleInputs) -> Vec<Hit> {
        let rows = &inputs.sandwiches;
        let Some(single_population) = sandwich_population(rows.iter().map(|r| {
            (
                r.sandwich_rate_30d,
                r.blocks_produced,
                r.blocks_with_sandwiches,
            )
        })) else {
            return vec![];
        };
        let wide_population = sandwich_population(rows.iter().filter_map(|r| {
            r.wide_sandwich_rate_30d
                .map(|wide| (wide, r.blocks_produced, wide_count(wide, r.blocks_produced)))
        }));

        let mut hits: Vec<Hit> = rows
            .iter()
            .filter_map(|row: &SandwichRow| {
                let single = sandwich_hit(
                    row.sandwich_rate_30d,
                    row.blocks_produced,
                    row.blocks_with_sandwiches,
                    &single_population,
                    self.rate_pct,
                )
                .map(|p_value| {
                    observed(
                        row.sandwich_rate_30d,
                        row.blocks_produced,
                        row.blocks_with_sandwiches,
                        &single_population,
                        p_value,
                    )
                });
                let wide = wide_population.as_ref().and_then(|population| {
                    let rate = row.wide_sandwich_rate_30d?;
                    let count = wide_count(rate, row.blocks_produced);
                    sandwich_hit(rate, row.blocks_produced, count, population, self.rate_pct).map(
                        |p_value| observed(rate, row.blocks_produced, count, population, p_value),
                    )
                });
                let kind = match (&single, &wide) {
                    (Some(_), _) => "SINGLE",
                    (None, Some(_)) => "WIDE",
                    (None, None) => return None,
                };
                Some(Hit {
                    vote_account: row.vote_account.clone(),
                    code: format!("MALICIOUS_SANDWICHED_ME_{kind}_{}", inputs.epoch),
                    epochs: vec![inputs.epoch],
                    evidence: json!({ "epoch": inputs.epoch, "single": single, "wide": wide }),
                })
            })
            .collect();
        hits.sort_by(|a, b| a.vote_account.cmp(&b.vote_account));
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const E: u64 = 1075;

    fn rule() -> Sandwich {
        Sandwich { rate_pct: 4.0 }
    }

    fn row(account: &str, blocks: u64, sandwiched: u64, wide: Option<f64>) -> SandwichRow {
        SandwichRow {
            vote_account: account.into(),
            blocks_produced: blocks,
            blocks_with_sandwiches: sandwiched,
            sandwich_rate_30d: (1000.0 * sandwiched as f64 / blocks as f64).round() / 10.0,
            wide_sandwich_rate_30d: wide,
        }
    }

    /// A cluster at ~0.77 % pooled and 0.8 % median, plus `rows`.
    fn evaluate(rows: Vec<SandwichRow>) -> Vec<Hit> {
        let mut sandwiches: Vec<SandwichRow> = (0..40)
            .map(|i| row(&format!("n{i:02}"), 5000, 38 + (i % 3), Some(0.8)))
            .collect();
        sandwiches.extend(rows);
        rule().evaluate(&RuleInputs {
            epoch: E,
            sandwiches,
            ..Default::default()
        })
    }

    fn codes(hits: &[Hit]) -> Vec<(&str, &str)> {
        hits.iter()
            .map(|hit| (hit.vote_account.as_str(), hit.code.as_str()))
            .collect()
    }

    #[test]
    fn a_chance_count_does_not_hit() {
        assert!(evaluate(vec![row("thin", 8, 2, None)]).is_empty());
    }

    #[test]
    fn a_significant_rate_over_the_floor_hits_single() {
        let hits = evaluate(vec![
            row("dhoz", 104, 7, None),
            row("blackmont", 196, 26, None),
        ]);
        assert_eq!(
            codes(&hits),
            vec![
                ("blackmont", "MALICIOUS_SANDWICHED_ME_SINGLE_1075"),
                ("dhoz", "MALICIOUS_SANDWICHED_ME_SINGLE_1075")
            ]
        );
        assert_eq!(hits[0].epochs, vec![E]);
    }

    #[test]
    fn a_rate_under_the_floor_does_not_hit() {
        assert!(evaluate(vec![row("big", 10_000, 390, None)]).is_empty());
    }

    #[test]
    fn a_wide_rate_hits_with_the_derived_count() {
        let hits = evaluate(vec![row("wide", 5000, 40, Some(6.0))]);
        assert_eq!(
            codes(&hits),
            vec![("wide", "MALICIOUS_SANDWICHED_ME_WIDE_1075")]
        );
        assert_eq!(hits[0].evidence["wide"]["blocks_with_sandwiches"], 300);
        assert_eq!(hits[0].evidence["single"], Value::Null);
    }

    #[test]
    fn single_takes_precedence_over_wide() {
        let hits = evaluate(vec![row("both", 5000, 400, Some(9.0))]);
        assert_eq!(
            codes(&hits),
            vec![("both", "MALICIOUS_SANDWICHED_ME_SINGLE_1075")]
        );
        assert!(hits[0].evidence["wide"].is_object());
    }

    #[test]
    fn no_wide_rows_means_no_wide_hits() {
        let hits = rule().evaluate(&RuleInputs {
            epoch: E,
            sandwiches: (0..10)
                .map(|i| row(&format!("n{i}"), 5000, 40, None))
                .collect(),
            ..Default::default()
        });
        assert!(hits.is_empty());
    }

    #[test]
    fn evidence_holds_observed_values_only() {
        let hits = evaluate(vec![row("both", 5000, 400, Some(9.0))]);
        let evidence = &hits[0].evidence;
        let mut keys: Vec<&String> = evidence["single"].as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "blocks_produced",
                "blocks_with_sandwiches",
                "cluster_median_pct",
                "cluster_pooled_pct",
                "p_value",
                "rate_30d_pct"
            ]
        );
        let text = evidence.to_string();
        assert!(
            !text.contains("\"bar\"") && !text.contains("\"rate_pct\""),
            "{text}"
        );
    }
}
