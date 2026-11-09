use super::config::{BlacklistConfig, ExemptEntry, ManualEntry, RuleId, RulesConfig};
use super::db::max_epoch;
use super::rules::{
    registry, required_inputs, window_start, Hit, Input, RuleInputs, SandwichRow, SlotTime,
};
use clap::Parser;
use collect::validators_slot_times::SOURCE_TRILLIUM;
use log::{info, warn};
use rust_decimal::prelude::*;
use std::collections::{BTreeMap, HashSet};
use tokio_postgres::{Client, Transaction};

#[derive(Debug, Parser)]
pub struct EvaluateParams {
    #[arg(long = "config", help = "The blacklist YAML config.")]
    config_path: String,
}

/// Syncs the hand-kept lists, then runs the rules over every closed epoch not evaluated yet. One
/// transaction: a failure leaves the table as it was.
pub async fn evaluate(params: EvaluateParams, psql_client: &mut Client) -> anyhow::Result<()> {
    let config = BlacklistConfig::load(&params.config_path)?;
    let tx = psql_client.transaction().await?;
    let current_epoch = max_epoch(&tx, "cluster_info")
        .await?
        .ok_or_else(|| anyhow::anyhow!("cluster_info is empty, no current epoch"))?;

    sync_manual(&tx, &config.manual, current_epoch).await?;
    apply_exempt(&tx, &config.exempt, current_epoch).await?;
    match config
        .rules
        .as_ref()
        .and_then(|rules| Some((rules, rules.start_epoch?)))
    {
        Some((rules, start_epoch)) => {
            run_rules(&tx, rules, start_epoch, &config.exempt, current_epoch).await?
        }
        None => info!("rules.start_epoch is not set, the rules are off"),
    }

    tx.commit().await?;
    Ok(())
}

async fn sync_manual(
    tx: &Transaction<'_>,
    manual: &[ManualEntry],
    current_epoch: u64,
) -> anyhow::Result<()> {
    let accounts: Vec<&str> = manual.iter().map(|e| e.vote_account.as_str()).collect();
    let codes: Vec<&str> = manual.iter().map(|e| e.code.as_str()).collect();
    let reasons: Vec<&str> = manual.iter().map(|e| e.reason.as_str()).collect();
    let epoch = Decimal::from(current_epoch);

    let added = tx
        .execute(
            "INSERT INTO blacklist (vote_account, source, code, reason, added_epoch, created_at, updated_at)
             SELECT vote_account, 'manual', code, reason, $4, now(), now()
             FROM UNNEST($1::TEXT[], $2::TEXT[], $3::TEXT[]) AS t(vote_account, code, reason)
             ON CONFLICT (vote_account, code) WHERE source = 'manual' AND removed_epoch IS NULL
             DO UPDATE SET reason = EXCLUDED.reason, updated_at = now()
             WHERE blacklist.reason IS DISTINCT FROM EXCLUDED.reason",
            &[&accounts, &codes, &reasons, &epoch],
        )
        .await?;
    let removed = tx
        .execute(
            "UPDATE blacklist
             SET removed_epoch = $3, removed_reason = 'removed from config', updated_at = now()
             WHERE source = 'manual' AND removed_epoch IS NULL
               AND NOT EXISTS (
                 SELECT 1 FROM UNNEST($1::TEXT[], $2::TEXT[]) AS t(vote_account, code)
                 WHERE t.vote_account = blacklist.vote_account AND t.code = blacklist.code
               )",
            &[&accounts, &codes, &epoch],
        )
        .await?;
    info!("Manual entries: {added} added or updated, {removed} removed");
    Ok(())
}

/// Closes rule and legacy rows an exemption covers. Never touches manual rows.
async fn apply_exempt(
    tx: &Transaction<'_>,
    exempt: &[ExemptEntry],
    current_epoch: u64,
) -> anyhow::Result<()> {
    let epoch = Decimal::from(current_epoch);
    for entry in exempt {
        let until_epoch = entry.until_epoch.map(Decimal::from);
        let closed = tx
            .execute(
                "UPDATE blacklist
                 SET removed_epoch = $1, removed_reason = 'exempt: ' || $2::TEXT, updated_at = now()
                 WHERE removed_epoch IS NULL AND vote_account = $3
                   AND (
                     (source = 'rule' AND ($4::TEXT = 'all' OR rule = $4::TEXT)
                       AND ($5::NUMERIC IS NULL OR evidence_from_epoch <= $5))
                     OR (source = 'legacy' AND $4::TEXT IN ('all', 'legacy'))
                   )",
                &[
                    &epoch,
                    &entry.reason,
                    &entry.vote_account,
                    &entry.rule.as_str(),
                    &until_epoch,
                ],
            )
            .await?;
        if closed > 0 {
            info!(
                "Exemption closed {closed} rows of {} ({})",
                entry.vote_account,
                entry.rule.as_str()
            );
        }
    }
    Ok(())
}

fn suppressed(
    hit: &Hit,
    rule: RuleId,
    active: &HashSet<(String, String)>,
    exempt: &[ExemptEntry],
) -> bool {
    active.contains(&(hit.vote_account.clone(), rule.as_str().to_string()))
        || exempt.iter().any(|entry| {
            entry.vote_account == hit.vote_account
                && (entry.rule == RuleId::All || entry.rule == rule)
                && entry
                    .until_epoch
                    .is_none_or(|until| hit.epochs.iter().any(|epoch| *epoch <= until))
        })
}

async fn run_rules(
    tx: &Transaction<'_>,
    rules_config: &RulesConfig,
    start_epoch: u64,
    exempt: &[ExemptEntry],
    current_epoch: u64,
) -> anyhow::Result<()> {
    let rules = registry(rules_config);
    if rules.is_empty() {
        info!("No rule is configured, nothing to evaluate");
        return Ok(());
    }
    let required = required_inputs(&rules);
    let from_epoch = max_epoch(tx, "blacklist_evaluations")
        .await?
        .map_or(start_epoch, |last| start_epoch.max(last + 1));
    let Some(last_closed) = max_epoch(tx, "epochs").await? else {
        info!("No closed epoch yet");
        return Ok(());
    };

    let mut active: HashSet<(String, String)> = tx
        .query(
            "SELECT vote_account, rule FROM blacklist WHERE source = 'rule' AND removed_epoch IS NULL",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| (row.get("vote_account"), row.get("rule")))
        .collect();

    for epoch in from_epoch..=last_closed {
        if !ready(tx, epoch, &required).await? {
            let mut later_ready = false;
            for later in epoch + 1..=last_closed {
                if ready(tx, later, &required).await? {
                    later_ready = true;
                    break;
                }
            }
            if !later_ready {
                info!("Inputs of epoch {epoch} are not complete yet, stopping");
                break;
            }
            warn!("Inputs of epoch {epoch} never arrived while a later epoch's did, skipping it");
            record_evaluation(tx, epoch, 0, true).await?;
            continue;
        }
        let inputs = load_inputs(tx, epoch, &required).await?;
        let mut rows_added: i32 = 0;
        for rule in &rules {
            for hit in rule.evaluate(&inputs) {
                if suppressed(&hit, rule.id(), &active, exempt) {
                    continue;
                }
                tx.execute(
                    "INSERT INTO blacklist (vote_account, source, rule, code, evidence,
                        evidence_from_epoch, added_epoch, created_at, updated_at)
                     VALUES ($1, 'rule', $2, $3, $4::TEXT::JSONB, $5, $6, now(), now())",
                    &[
                        &hit.vote_account,
                        &rule.id().as_str(),
                        &hit.code,
                        &hit.evidence.to_string(),
                        &hit.epochs.iter().min().map(|e| Decimal::from(*e)),
                        &Decimal::from(current_epoch),
                    ],
                )
                .await?;
                info!("Epoch {epoch}: {} listed as {}", hit.vote_account, hit.code);
                active.insert((hit.vote_account, rule.id().as_str().to_string()));
                rows_added += 1;
            }
        }
        record_evaluation(tx, epoch, rows_added, false).await?;
        info!("Epoch {epoch} evaluated: {rows_added} rows added");
    }
    Ok(())
}

async fn record_evaluation(
    tx: &Transaction<'_>,
    epoch: u64,
    rows_added: i32,
    skipped: bool,
) -> anyhow::Result<()> {
    tx.execute(
        "INSERT INTO blacklist_evaluations (epoch, rows_added, skipped, evaluated_at)
         VALUES ($1, $2, $3, now())",
        &[&Decimal::from(epoch), &rows_added, &skipped],
    )
    .await?;
    Ok(())
}

/// The table and the filter that hold one input of an epoch.
fn input_source(input: Input) -> (&'static str, String) {
    match input {
        Input::SlotTimes => (
            "validators_slot_times",
            format!("source = '{SOURCE_TRILLIUM}'"),
        ),
        Input::Stakes => ("validators", "TRUE".into()),
        Input::Sandwiches => ("validators_sandwiches", "TRUE".into()),
    }
}

/// Whether every input the rules read is stored for every epoch of its window, so a
/// half-collected epoch is never judged.
async fn ready(
    tx: &Transaction<'_>,
    epoch: u64,
    required: &BTreeMap<Input, u64>,
) -> anyhow::Result<bool> {
    let closed: bool = tx
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM epochs WHERE epoch = $1) AS closed",
            &[&Decimal::from(epoch)],
        )
        .await?
        .get("closed");
    if !closed {
        return Ok(false);
    }
    for (input, window) in required {
        let (table, filter) = input_source(*input);
        let from = window_start(epoch, *window);
        let stored: i64 = tx
            .query_one(
                &format!(
                    "SELECT COUNT(DISTINCT epoch) AS stored FROM {table}
                     WHERE {filter} AND epoch BETWEEN $1 AND $2"
                ),
                &[&Decimal::from(from), &Decimal::from(epoch)],
            )
            .await?
            .get("stored");
        if stored as u64 != epoch - from + 1 {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn load_inputs(
    tx: &Transaction<'_>,
    epoch: u64,
    required: &BTreeMap<Input, u64>,
) -> anyhow::Result<RuleInputs> {
    let mut inputs = RuleInputs {
        epoch,
        ..Default::default()
    };
    for (input, window) in required {
        let (table, filter) = input_source(*input);
        let range = [
            Decimal::from(window_start(epoch, *window)),
            Decimal::from(epoch),
        ];
        let rows = tx
            .query(
                &format!("SELECT * FROM {table} WHERE {filter} AND epoch BETWEEN $1 AND $2"),
                &[&range[0], &range[1]],
            )
            .await?;
        for row in rows {
            let row_epoch: u64 = row.get::<_, Decimal>("epoch").try_into()?;
            match input {
                Input::SlotTimes => {
                    inputs
                        .slot_times
                        .entry(row_epoch)
                        .or_default()
                        .push(SlotTime {
                            vote_account: row.get("vote_account"),
                            slot_duration_median_ms: row.get("slot_duration_median_ms"),
                        })
                }
                Input::Stakes => {
                    inputs.stakes.entry(row_epoch).or_default().insert(
                        row.get("vote_account"),
                        row.get::<_, Decimal>("activated_stake").try_into()?,
                    );
                }
                Input::Sandwiches => inputs.sandwiches.push(SandwichRow {
                    vote_account: row.get("vote_account"),
                    blocks_produced: row.get::<_, Decimal>("blocks_produced").try_into()?,
                    blocks_with_sandwiches: row
                        .get::<_, Decimal>("blocks_with_sandwiches")
                        .try_into()?,
                    sandwich_rate_30d: row.get("sandwich_rate_30d"),
                    wide_sandwich_rate_30d: row.get("wide_sandwich_rate_30d"),
                }),
            }
        }
    }
    Ok(inputs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hit(account: &str, epochs: Vec<u64>) -> Hit {
        Hit {
            vote_account: account.into(),
            code: "SLOW_SLOTS_1080".into(),
            epochs,
            evidence: json!({}),
        }
    }

    fn exempt(yaml: &str) -> Vec<ExemptEntry> {
        BlacklistConfig::parse(yaml).unwrap().exempt
    }

    #[test]
    fn an_active_row_suppresses_a_second_one() {
        let active = HashSet::from([(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string(),
            "slow_slots".to_string(),
        )]);
        assert!(suppressed(
            &hit("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", vec![1080]),
            RuleId::SlowSlots,
            &active,
            &[]
        ));
        assert!(!suppressed(
            &hit("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", vec![1080]),
            RuleId::Sandwich,
            &active,
            &[]
        ));
    }

    #[test]
    fn a_permanent_exemption_suppresses_its_rule_or_all() {
        let none = HashSet::new();
        let all = exempt("exempt: [{ vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, reason: R }]");
        let sandwich = exempt("exempt: [{ vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, rule: sandwich, reason: R }]");
        assert!(suppressed(
            &hit("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", vec![1080]),
            RuleId::SlowSlots,
            &none,
            &all
        ));
        assert!(!suppressed(
            &hit("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", vec![1080]),
            RuleId::SlowSlots,
            &none,
            &sandwich
        ));
        assert!(!suppressed(
            &hit("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB", vec![1080]),
            RuleId::SlowSlots,
            &none,
            &all
        ));
    }

    #[test]
    fn until_epoch_ignores_only_evidence_up_to_it() {
        let none = HashSet::new();
        let until =
            exempt("exempt: [{ vote_account: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, rule: slow_slots, until_epoch: 1083, reason: R }]");
        assert!(suppressed(
            &hit("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", vec![1083, 1084]),
            RuleId::SlowSlots,
            &none,
            &until
        ));
        assert!(!suppressed(
            &hit("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", vec![1084, 1085]),
            RuleId::SlowSlots,
            &none,
            &until
        ));
    }
}
