use super::config::{BlacklistConfig, ExemptEntry, ManualEntry, RuleId, RulesConfig};
use super::db::last_evaluated_epoch;
use super::rules::{registry, Hit, RuleInputs, SandwichRow, SlotTime};
use clap::Parser;
use log::info;
use rust_decimal::prelude::*;
use std::collections::HashSet;
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
    let current_epoch = epoch_of(&tx, "SELECT MAX(epoch) AS epoch FROM cluster_info")
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

async fn epoch_of(tx: &Transaction<'_>, query: &str) -> anyhow::Result<Option<u64>> {
    Ok(tx
        .query_one(query, &[])
        .await?
        .get::<_, Option<Decimal>>("epoch")
        .map(u64::try_from)
        .transpose()?)
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
    let window = rules
        .iter()
        .map(|rule| rule.epochs_window())
        .max()
        .unwrap_or(1);
    let from_epoch = last_evaluated_epoch(tx)
        .await?
        .map_or(start_epoch, |last| start_epoch.max(last + 1));
    let Some(last_closed) = epoch_of(tx, "SELECT MAX(epoch) AS epoch FROM epochs").await? else {
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
        let Some(inputs) = load_inputs(tx, epoch, window).await? else {
            info!("Inputs of epoch {epoch} are not complete yet, stopping");
            break;
        };
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
        tx.execute(
            "INSERT INTO blacklist_evaluations (epoch, rows_added, evaluated_at) VALUES ($1, $2, now())",
            &[&Decimal::from(epoch), &rows_added],
        )
        .await?;
        info!("Epoch {epoch} evaluated: {rows_added} rows added");
    }
    Ok(())
}

/// `None` until every input of `epoch` is stored, so a half-collected epoch is never judged.
async fn load_inputs(
    tx: &Transaction<'_>,
    epoch: u64,
    window: u64,
) -> anyhow::Result<Option<RuleInputs>> {
    let at = Decimal::from(epoch);
    let ready: bool = tx
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM epochs WHERE epoch = $1)
                AND EXISTS (SELECT 1 FROM validators_slot_times WHERE epoch = $1)
                AND EXISTS (SELECT 1 FROM validators_sandwiches WHERE epoch = $1)
                AND EXISTS (SELECT 1 FROM validators WHERE epoch = $1) AS ready",
            &[&at],
        )
        .await?
        .get("ready");
    if !ready {
        return Ok(None);
    }

    let from = Decimal::from(epoch + 1 - window.min(epoch + 1));
    let mut inputs = RuleInputs {
        epoch,
        ..Default::default()
    };
    for row in tx
        .query(
            "SELECT epoch, vote_account, slot_duration_median_ms FROM validators_slot_times
             WHERE epoch BETWEEN $1 AND $2",
            &[&from, &at],
        )
        .await?
    {
        inputs
            .slot_times
            .entry(row.get::<_, Decimal>("epoch").try_into()?)
            .or_default()
            .push(SlotTime {
                vote_account: row.get("vote_account"),
                slot_duration_median_ms: row.get("slot_duration_median_ms"),
            });
    }
    for row in tx
        .query(
            "SELECT epoch, vote_account, activated_stake FROM validators WHERE epoch BETWEEN $1 AND $2",
            &[&from, &at],
        )
        .await?
    {
        inputs
            .stakes
            .entry(row.get::<_, Decimal>("epoch").try_into()?)
            .or_default()
            .insert(
                row.get("vote_account"),
                row.get::<_, Decimal>("activated_stake").try_into()?,
            );
    }
    for row in tx
        .query(
            "SELECT vote_account, blocks_produced, blocks_with_sandwiches, sandwich_rate_30d,
                wide_sandwich_rate_30d
             FROM validators_sandwiches WHERE epoch = $1",
            &[&at],
        )
        .await?
    {
        inputs.sandwiches.push(SandwichRow {
            vote_account: row.get("vote_account"),
            blocks_produced: row.get::<_, Decimal>("blocks_produced").try_into()?,
            blocks_with_sandwiches: row.get::<_, Decimal>("blocks_with_sandwiches").try_into()?,
            sandwich_rate_30d: row.get("sandwich_rate_30d"),
            wide_sandwich_rate_30d: row.get("wide_sandwich_rate_30d"),
        });
    }
    Ok(Some(inputs))
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
        let active = HashSet::from([("A".to_string(), "slow_slots".to_string())]);
        assert!(suppressed(
            &hit("A", vec![1080]),
            RuleId::SlowSlots,
            &active,
            &[]
        ));
        assert!(!suppressed(
            &hit("A", vec![1080]),
            RuleId::Sandwich,
            &active,
            &[]
        ));
    }

    #[test]
    fn a_permanent_exemption_suppresses_its_rule_or_all() {
        let none = HashSet::new();
        let all = exempt("exempt: [{ vote_account: A, reason: R }]");
        let sandwich = exempt("exempt: [{ vote_account: A, rule: sandwich, reason: R }]");
        assert!(suppressed(
            &hit("A", vec![1080]),
            RuleId::SlowSlots,
            &none,
            &all
        ));
        assert!(!suppressed(
            &hit("A", vec![1080]),
            RuleId::SlowSlots,
            &none,
            &sandwich
        ));
        assert!(!suppressed(
            &hit("B", vec![1080]),
            RuleId::SlowSlots,
            &none,
            &all
        ));
    }

    #[test]
    fn until_epoch_ignores_only_evidence_up_to_it() {
        let none = HashSet::new();
        let until =
            exempt("exempt: [{ vote_account: A, rule: slow_slots, until_epoch: 1083, reason: R }]");
        assert!(suppressed(
            &hit("A", vec![1083, 1084]),
            RuleId::SlowSlots,
            &none,
            &until
        ));
        assert!(!suppressed(
            &hit("A", vec![1084, 1085]),
            RuleId::SlowSlots,
            &none,
            &until
        ));
    }
}
