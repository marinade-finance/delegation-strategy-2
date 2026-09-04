use crate::dto::{
    client_label, client_lineage, effective_client_id, GroupIncidents, ValidatorEpochStats,
    ValidatorGroupNode, ValidatorGroupRecord, ValidatorGroupTree, ValidatorGroups, ValidatorRecord,
};
use crate::operators;
use crate::stake_deltas::delta_epochs;
use crate::utils::{is_eligible_validator, last_reported_epoch, worst_known_commission};
use rust_decimal::prelude::*;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum GroupKind {
    /// The client with the block engine it runs, `Agave + Jito`.
    ClientLabel,
    /// The client alone, `Agave`. The parent level of the client tree.
    ClientLineage,
    /// The hosting organisation, `Hetzner`.
    ProviderAso,
    /// The node operator, `Figment`, from the operators CSV.
    Operator,
}

impl GroupKind {
    /// A vote account absent from the operators CSV belongs to no operator, where a missing client or
    /// provider still describes a validator that is running somewhere.
    fn drops_unclassified(self) -> bool {
        matches!(self, GroupKind::Operator)
    }

    fn carries_incidents_as_records(self) -> bool {
        matches!(self, GroupKind::Operator)
    }
}

/// Key of the bucket holding validators whose value is unknown.
pub const UNKNOWN_GROUP: &str = "Unknown";

fn normalized(value: Option<String>) -> Option<String> {
    let value = value?;
    let value = value.trim();
    if value.is_empty() || is_unknown_placeholder(value) {
        return None;
    }

    Some(value.to_string())
}

fn is_unknown_placeholder(value: &str) -> bool {
    let lowercase = value.to_lowercase();
    if lowercase == "unknown" {
        return true;
    }

    match lowercase
        .strip_prefix("unknown(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        Some(id) => !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// The registry renders a client lowercase (`agave`); block engine labels are title-cased.
fn as_client_name(client: String) -> String {
    let mut characters = client.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => client,
    }
}

fn group_key(
    validator: &ValidatorRecord,
    stats: &ValidatorEpochStats,
    kind: GroupKind,
) -> Option<String> {
    let client_id = effective_client_id(stats.client_id, stats.client_id_raw.as_deref());

    match kind {
        GroupKind::Operator => {
            normalized(operators::operator_of(&validator.vote_account).map(str::to_string))
        }
        GroupKind::ProviderAso => normalized(stats.dc_aso.clone()),
        // The raw rendering is what the node reported, kept for a client absent from client-ids.csv.
        GroupKind::ClientLabel => normalized(Some(client_label(client_id)))
            .or_else(|| normalized(stats.client_id_raw.clone())),
        GroupKind::ClientLineage => normalized(client_lineage(client_id)).map(as_client_name),
    }
}

/// Keys for the epoch being served.
fn current_group_key(
    validator: &ValidatorRecord,
    stats: &ValidatorEpochStats,
    kind: GroupKind,
) -> Option<String> {
    match kind {
        GroupKind::ClientLabel => normalized(Some(validator.client_label.clone()))
            .or_else(|| normalized(validator.client_id_raw.clone())),
        GroupKind::ClientLineage => {
            normalized(validator.client_lineage.clone()).map(as_client_name)
        }
        // Neither is projected onto the record, so both read the epoch as stored.
        GroupKind::Operator | GroupKind::ProviderAso => group_key(validator, stats, kind),
    }
}

/// Case-folded group identity; the geolocation source re-cases provider names between epochs.
type FoldedKey = Option<String>;

fn folded(key: &Option<String>) -> FoldedKey {
    key.as_ref().map(|key| key.to_lowercase())
}

/// A member without the value is left out of both sums, so it neither dilutes the mean nor reads as zero.
#[derive(Default)]
struct StakeWeighted {
    weighted: f64,
    weight: f64,
}

impl StakeWeighted {
    fn add(&mut self, value: Option<f64>, weight: f64) {
        if let Some(value) = value.filter(|value| value.is_finite()) {
            self.weighted += value * weight;
            self.weight += weight;
        }
    }

    fn mean(&self) -> Option<f64> {
        (self.weight > 0.0).then(|| self.weighted / self.weight)
    }
}

// Not `Default`: `incidents` takes its shape from the kind.
struct Accumulator {
    spellings: HashMap<String, Decimal>,
    validator_count: u64,
    total_stake: Decimal,
    net_apy: StakeWeighted,
    take_rate: StakeWeighted,
    credits: StakeWeighted,
    marinade_score: StakeWeighted,
    apy: StakeWeighted,
    commission: StakeWeighted,
    uptime_pct: StakeWeighted,
    expected_take_rate: StakeWeighted,
    delegation_relationship_count: Option<u64>,
    incidents: GroupIncidents,
}

impl Accumulator {
    fn new(kind: GroupKind) -> Self {
        Self {
            spellings: Default::default(),
            validator_count: 0,
            total_stake: Decimal::ZERO,
            net_apy: Default::default(),
            take_rate: Default::default(),
            credits: Default::default(),
            marinade_score: Default::default(),
            apy: Default::default(),
            commission: Default::default(),
            uptime_pct: Default::default(),
            expected_take_rate: Default::default(),
            delegation_relationship_count: None,
            incidents: if kind.carries_incidents_as_records() {
                GroupIncidents::empty_records()
            } else {
                GroupIncidents::empty_count()
            },
        }
    }

    fn name(&self) -> Option<String> {
        self.spellings
            .iter()
            .max_by(|(a_spelling, a_stake), (b_spelling, b_stake)| {
                a_stake
                    .cmp(b_stake)
                    .then_with(|| b_spelling.cmp(a_spelling))
            })
            .map(|(spelling, _)| spelling.clone())
    }

    fn add(
        &mut self,
        validator: &ValidatorRecord,
        stats: &ValidatorEpochStats,
        name: Option<&String>,
    ) {
        self.validator_count += 1;
        self.total_stake += stats.activated_stake;
        self.incidents
            .add(&validator.vote_account, &validator.incidents);

        if let Some(name) = name {
            *self.spellings.entry(name.clone()).or_default() += stats.activated_stake;
        }

        let weight = stats.activated_stake.to_f64().unwrap_or_default();

        self.net_apy.add(validator.net_apy, weight);
        self.take_rate.add(validator.avg_take_rate, weight);
        self.credits.add(Some(validator.credits as f64), weight);
        self.marinade_score.add(validator.score, weight);
        self.apy.add(validator.avg_apy, weight);
        // Left out when unknown on both sides, where the per-validator column reads the worst case.
        self.commission.add(
            worst_known_commission(
                validator.commission_max_observed,
                validator.commission_advertised,
            )
            .map(|commission| commission as f64),
            weight,
        );
        self.uptime_pct.add(validator.avg_uptime_pct, weight);
        self.expected_take_rate
            .add(validator.expected_take_rate, weight);

        // Each member's own distinct count, so an authority delegating to several members of the
        // group lands in the sum once per validator.
        if let Some(unique_delegators) = validator.unique_delegators {
            self.delegation_relationship_count =
                Some(self.delegation_relationship_count.unwrap_or_default() + unique_delegators);
        }
    }

    fn finish(
        self,
        folded_key: &FoldedKey,
        total_activated_stake: Decimal,
        baseline_7d: Option<&ReferenceStake>,
        baseline_30d: Option<&ReferenceStake>,
    ) -> ValidatorGroupRecord {
        let delta = |reference: Option<&ReferenceStake>| {
            reference.map(|group_stake| {
                self.total_stake - group_stake.get(folded_key).copied().unwrap_or_default()
            })
        };

        ValidatorGroupRecord {
            key: self.name().unwrap_or_else(|| UNKNOWN_GROUP.to_string()),
            validator_count: self.validator_count,
            total_stake: self.total_stake,
            stake_share: if total_activated_stake.is_zero() {
                0.0
            } else {
                (self.total_stake / total_activated_stake)
                    .to_f64()
                    .unwrap_or_default()
            },
            stake_delta_7d: delta(baseline_7d),
            stake_delta_30d: delta(baseline_30d),
            net_apy: self.net_apy.mean(),
            take_rate: self.take_rate.mean(),
            credits: self.credits.mean(),
            marinade_score: self.marinade_score.mean(),
            apy: self.apy.mean(),
            commission: self.commission.mean(),
            uptime_pct: self.uptime_pct.mean(),
            expected_take_rate: self.expected_take_rate.mean(),
            delegation_relationship_count: self.delegation_relationship_count,
            incidents: {
                let mut incidents = self.incidents;
                incidents.sort();
                incidents
            },
        }
    }
}

/// The row a validator belonging to no group stands for on its own. Ordered against the aggregated
/// rows, so it has to read the same fields `Accumulator::add` does.
pub fn singleton_group(validator: &ValidatorRecord) -> ValidatorGroupRecord {
    // Dropped the way `StakeWeighted::add` drops them.
    let finite = |value: Option<f64>| value.filter(|value: &f64| value.is_finite());

    ValidatorGroupRecord {
        key: validator
            .info_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&validator.vote_account)
            .to_string(),
        validator_count: 1,
        total_stake: validator.activated_stake,
        stake_share: 0.0,
        stake_delta_7d: validator.stake_delta_7d,
        stake_delta_30d: validator.stake_delta_30d,
        net_apy: finite(validator.net_apy),
        take_rate: finite(validator.avg_take_rate),
        credits: Some(validator.credits as f64),
        marinade_score: finite(validator.score),
        apy: finite(validator.avg_apy),
        commission: worst_known_commission(
            validator.commission_max_observed,
            validator.commission_advertised,
        )
        .map(|commission| commission as f64),
        uptime_pct: finite(validator.avg_uptime_pct),
        expected_take_rate: finite(validator.expected_take_rate),
        delegation_relationship_count: validator.unique_delegators,
        // `top_level_ranks` reads nothing off this row but the sort key and the name.
        incidents: GroupIncidents::Count(validator.incidents.len() as u64),
    }
}

type ReferenceStake = HashMap<FoldedKey, Decimal>;

/// Stake each `kind` group held in `epoch`, bucketed by the keys that epoch's own rows resolve to.
/// `None` when none of them resolved a key.
fn group_stake_at(
    validators: &[&ValidatorRecord],
    epoch: u64,
    kind: GroupKind,
) -> Option<ReferenceStake> {
    let mut group_stake: ReferenceStake = Default::default();
    for validator in validators {
        if let Some(stats) = validator.epoch_stats.iter().find(|s| s.epoch == epoch) {
            *group_stake
                .entry(folded(&group_key(validator, stats, kind)))
                .or_default() += stats.activated_stake;
        }
    }

    group_stake
        .keys()
        .any(|key| key.is_some())
        .then_some(group_stake)
}

/// One grouping on its own, which only the tests read; `aggregate_all` is what the cache warms.
#[cfg(test)]
fn aggregate_groups(
    validators: &HashMap<String, ValidatorRecord>,
    kind: GroupKind,
) -> ValidatorGroups {
    let Some(population) = Population::new(validators) else {
        return Default::default();
    };

    aggregate_kind(&population, kind)
}

/// Groups are built from `eligible`; the delta baselines are measured over `all`, so stake that has
/// since left the eligible set still shows as a loss.
struct Population<'a> {
    eligible: Vec<&'a ValidatorRecord>,
    all: Vec<&'a ValidatorRecord>,
    current_epoch: u64,
    /// Denominator behind every grouping's `stake_share`. Taken over the whole eligible set rather
    /// than over the rows, so a grouping that drops its unclassified members still reports shares
    /// of the cluster.
    eligible_stake: Decimal,
}

impl<'a> Population<'a> {
    fn new(validators: &'a HashMap<String, ValidatorRecord>) -> Option<Self> {
        Self::over(eligible(validators), validators.values().collect())
    }

    /// `rows` are the validators the rows describe; `baselines` also carries the ones only the stake
    /// deltas read, which is every validator the cache holds when the rows describe the whole set.
    fn over(rows: Vec<&'a ValidatorRecord>, baselines: Vec<&'a ValidatorRecord>) -> Option<Self> {
        let eligible = rows;
        let current_epoch = eligible
            .iter()
            .flat_map(|validator| &validator.epoch_stats)
            .map(|stats| stats.epoch)
            .max()?;
        let eligible_stake = eligible
            .iter()
            .filter_map(|validator| {
                validator
                    .epoch_stats
                    .iter()
                    .find(|stats| stats.epoch == current_epoch)
            })
            .map(|stats| stats.activated_stake)
            .sum();

        Some(Self {
            eligible,
            all: baselines,
            current_epoch,
            eligible_stake,
        })
    }
}

/// Aggregates only the validators passed in, not the whole cluster: `stake_share` is a share of
/// them, and the stake deltas read the same list on both sides, so a validator missing from it does
/// not show up as a loss.
pub fn aggregate_operators(validators: &[&ValidatorRecord]) -> ValidatorGroups {
    match Population::over(validators.to_vec(), validators.to_vec()) {
        Some(population) => aggregate_kind(&population, GroupKind::Operator),
        None => Default::default(),
    }
}

/// The population `/validators` serves, judged against the newest epoch the whole cache reports.
fn eligible(validators: &HashMap<String, ValidatorRecord>) -> Vec<&ValidatorRecord> {
    let last_epoch = last_reported_epoch(validators.values()).unwrap_or(0);
    validators
        .values()
        .filter(|validator| is_eligible_validator(validator, last_epoch))
        .collect()
}

/// Rows with the folded key each was bucketed under; the client tree joins its two levels on it.
struct KeyedGroups {
    rows: Vec<(FoldedKey, ValidatorGroupRecord)>,
    groups: ValidatorGroups,
}

fn aggregate_kind(population: &Population, kind: GroupKind) -> ValidatorGroups {
    aggregate_keyed(population, kind).groups
}

fn aggregate_keyed(population: &Population, kind: GroupKind) -> KeyedGroups {
    let current_epoch = population.current_epoch;
    let (delta_7d_epoch, delta_30d_epoch) = delta_epochs(population.all.iter().copied());
    let baseline_7d = delta_7d_epoch.and_then(|epoch| group_stake_at(&population.all, epoch, kind));
    let baseline_30d =
        delta_30d_epoch.and_then(|epoch| group_stake_at(&population.all, epoch, kind));

    let mut accumulators: HashMap<FoldedKey, Accumulator> = Default::default();
    for validator in &population.eligible {
        let Some(stats) = validator
            .epoch_stats
            .iter()
            .find(|stats| stats.epoch == current_epoch)
        else {
            continue;
        };

        let key = current_group_key(validator, stats, kind);
        if key.is_none() && kind.drops_unclassified() {
            continue;
        }

        accumulators
            .entry(folded(&key))
            .or_insert_with(|| Accumulator::new(kind))
            .add(validator, stats, key.as_ref());
    }

    let total_activated_stake = population.eligible_stake;

    let mut rows: Vec<_> = accumulators
        .into_iter()
        .map(|(folded_key, accumulator)| {
            let record = accumulator.finish(
                &folded_key,
                total_activated_stake,
                baseline_7d.as_ref(),
                baseline_30d.as_ref(),
            );
            (folded_key, record)
        })
        .collect();

    // HashMap iteration order changes on every cache refresh; paged reads need a total order.
    rows.sort_by(|(_, a), (_, b)| {
        b.total_stake
            .cmp(&a.total_stake)
            .then_with(|| a.key.to_lowercase().cmp(&b.key.to_lowercase()))
            .then_with(|| a.key.cmp(&b.key))
    });

    KeyedGroups {
        groups: ValidatorGroups {
            groups: rows.iter().map(|(_, record)| record.clone()).collect(),
            total_activated_stake,
            current_epoch: Some(current_epoch),
        },
        rows,
    }
}

fn aggregate_client_tree(population: &Population) -> ValidatorGroupTree {
    let clients = aggregate_keyed(population, GroupKind::ClientLineage);
    let block_engines = aggregate_keyed(population, GroupKind::ClientLabel);
    let engines_by_client = block_engines_by_client(&population.eligible, population.current_epoch);

    let nodes = clients
        .rows
        .into_iter()
        .map(|(folded_client, client)| {
            let engines = engines_by_client.get(&folded_client);

            ValidatorGroupNode {
                children: block_engines
                    .rows
                    .iter()
                    .filter(|(folded_engine, _)| {
                        engines.is_some_and(|engines| engines.contains(folded_engine))
                    })
                    .map(|(_, engine)| engine.clone())
                    .collect(),
                group: client,
            }
        })
        .collect();

    ValidatorGroupTree {
        nodes,
        total_activated_stake: clients.groups.total_activated_stake,
        current_epoch: clients.groups.current_epoch,
    }
}

fn block_engines_by_client(
    validators: &[&ValidatorRecord],
    current_epoch: u64,
) -> HashMap<FoldedKey, HashSet<FoldedKey>> {
    let mut engines: HashMap<FoldedKey, HashSet<FoldedKey>> = Default::default();
    for validator in validators {
        if let Some(stats) = validator
            .epoch_stats
            .iter()
            .find(|stats| stats.epoch == current_epoch)
        {
            engines
                .entry(folded(&current_group_key(
                    validator,
                    stats,
                    GroupKind::ClientLineage,
                )))
                .or_default()
                .insert(folded(&current_group_key(
                    validator,
                    stats,
                    GroupKind::ClientLabel,
                )));
        }
    }
    engines
}

pub fn aggregate_all(validators: &HashMap<String, ValidatorRecord>) -> ValidatorGroupings {
    let Some(population) = Population::new(validators) else {
        return Default::default();
    };

    ValidatorGroupings {
        clients: aggregate_client_tree(&population),
        providers: aggregate_kind(&population, GroupKind::ProviderAso),
    }
}

#[derive(Default, Clone)]
pub struct ValidatorGroupings {
    pub clients: ValidatorGroupTree,
    pub providers: ValidatorGroups,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{client_name, client_vendor};
    use chrono::{DateTime, Duration, Utc};

    const EPOCH_SECONDS: i64 = 2 * 24 * 3600;

    fn epoch_end(epoch: u64, last_epoch: u64, now: DateTime<Utc>) -> DateTime<Utc> {
        now - Duration::seconds((last_epoch - epoch) as i64 * EPOCH_SECONDS)
    }

    /// `(epoch, stake, client_id, dc_aso)`.
    type EpochSpec = (u64, i64, Option<u16>, Option<&'static str>);

    const CURRENT_EPOCH: u64 = 100;
    const PREVIOUS_EPOCH: u64 = 99;

    /// The two epochs `is_eligible_validator` reads, both carrying the same stake and client.
    fn last_two_epochs(
        stake: i64,
        client_id: Option<u16>,
        dc_aso: Option<&'static str>,
    ) -> Vec<EpochSpec> {
        vec![
            (CURRENT_EPOCH, stake, client_id, dc_aso),
            (PREVIOUS_EPOCH, stake, client_id, dc_aso),
        ]
    }

    struct Member {
        vote_account: &'static str,
        /// Newest epoch first.
        epochs: Vec<EpochSpec>,
        net_apy: Option<f64>,
        take_rate: Option<f64>,
        credits: u64,
        marinade_score: Option<f64>,
        apy: Option<f64>,
        commission_max_observed: Option<i32>,
        commission_advertised: Option<i32>,
        uptime_pct: Option<f64>,
        expected_take_rate: Option<f64>,
        unique_delegators: Option<u64>,
        client_id_raw: Option<&'static str>,
        /// Days ago each incident interval began. Each lasts long enough to clear any floor.
        incidents_days_ago: Vec<i64>,
    }

    impl Member {
        fn new(vote_account: &'static str, epochs: Vec<EpochSpec>) -> Self {
            Self {
                vote_account,
                epochs,
                net_apy: None,
                take_rate: None,
                credits: 0,
                marinade_score: None,
                apy: None,
                commission_max_observed: None,
                commission_advertised: None,
                uptime_pct: None,
                expected_take_rate: None,
                unique_delegators: None,
                client_id_raw: None,
                incidents_days_ago: Vec::new(),
            }
        }
    }

    fn validators(members: Vec<Member>) -> HashMap<String, ValidatorRecord> {
        let last_epoch = members
            .iter()
            .flat_map(|member| member.epochs.iter().map(|(epoch, ..)| *epoch))
            .max()
            .unwrap_or_default();
        // One instant for every row: the delta windows land on exact epoch boundaries.
        let now = Utc::now();

        members
            .into_iter()
            .map(|member| {
                let epoch_stats = member
                    .epochs
                    .iter()
                    .map(|(epoch, stake, client_id, dc_aso)| ValidatorEpochStats {
                        epoch: *epoch,
                        epoch_end_at: Some(epoch_end(*epoch, last_epoch, now)),
                        activated_stake: Decimal::from(*stake),
                        client_id: *client_id,
                        client_id_raw: member.client_id_raw.map(str::to_string),
                        dc_aso: dc_aso.map(str::to_string),
                        ..Default::default()
                    })
                    .collect();

                let incidents: Vec<_> = member
                    .incidents_days_ago
                    .iter()
                    .map(|days_ago| crate::dto::IncidentRecord {
                        epoch: CURRENT_EPOCH,
                        detail: crate::dto::IncidentDetail::Downtime {
                            start_at: Utc::now() - Duration::days(*days_ago),
                            end_at: Utc::now() - Duration::days(*days_ago),
                            downtime_seconds: 600,
                            block_production: None,
                        },
                    })
                    .collect();

                // `load_validators` projects the node columns off the newest row that reported
                // any of them, so the record keeps a client the epoch being served has not
                // observed yet. The fixture's raw rendering is the member's, so a row counts as
                // reporting when it carries either half.
                let projected_client_id = member
                    .epochs
                    .iter()
                    .find(|(_, _, client_id, _)| {
                        client_id.is_some() || member.client_id_raw.is_some()
                    })
                    .and_then(|(_, _, client_id, _)| *client_id);

                (
                    member.vote_account.to_string(),
                    ValidatorRecord {
                        vote_account: member.vote_account.to_string(),
                        client_id: projected_client_id,
                        client_id_raw: member.client_id_raw.map(str::to_string),
                        client_name: client_name(projected_client_id),
                        client_label: client_label(projected_client_id),
                        client_vendor: client_vendor(projected_client_id),
                        client_lineage: client_lineage(projected_client_id),
                        epoch_stats,
                        net_apy: member.net_apy,
                        avg_take_rate: member.take_rate,
                        credits: member.credits,
                        score: member.marinade_score,
                        avg_apy: member.apy,
                        commission_max_observed: member.commission_max_observed,
                        commission_advertised: member.commission_advertised,
                        avg_uptime_pct: member.uptime_pct,
                        expected_take_rate: member.expected_take_rate,
                        unique_delegators: member.unique_delegators,
                        incidents,
                        ..Default::default()
                    },
                )
            })
            .collect()
    }

    fn keys(groups: &ValidatorGroups) -> Vec<String> {
        groups
            .groups
            .iter()
            .map(|group| group.key.clone())
            .collect()
    }

    fn group<'a>(groups: &'a ValidatorGroups, name: &str) -> &'a ValidatorGroupRecord {
        groups
            .groups
            .iter()
            .find(|group| group.key == name)
            .unwrap_or_else(|| panic!("no group for {name:?} in {:?}", keys(groups)))
    }

    // client-ids.csv ids: 3 `Agave`, 2 `Frankendancer`, 6 `Agave + JitoBAM` (agave lineage, like 3).
    /// `operators.default.csv` rows, so the grouping is exercised on a real mapping.
    const FIGMENT_ONE: &str = "CcaHc2L43ZWjwCHART3oZoJvHLAe9hzT2DJNUpBzoTN1";
    const FIGMENT_TWO: &str = "26pV97Ce83ZQ6Kz9XT4td8tdoUFPTng8Fb8gPyc53dJx";
    const HELIUS: &str = "he1iusunGwqrNtafDtLdhsUQDFvo13z9sUa36PauBtk";

    fn operators(validators: &HashMap<String, ValidatorRecord>) -> ValidatorGroups {
        aggregate_groups(validators, GroupKind::Operator)
    }

    const AGAVE: Option<u16> = Some(3);
    const FRANKENDANCER: Option<u16> = Some(2);
    const JITO_BAM: Option<u16> = Some(6);

    #[test]
    fn providers_group_by_the_hosting_organisation() {
        let validators = validators(vec![
            Member::new("one", last_two_epochs(300, AGAVE, Some("Hetzner"))),
            Member::new("two", last_two_epochs(200, FRANKENDANCER, Some("Hetzner"))),
            Member::new("three", last_two_epochs(100, AGAVE, Some("Latitude"))),
        ]);

        let groups = aggregate_groups(&validators, GroupKind::ProviderAso);
        assert_eq!(
            keys(&groups),
            vec!["Hetzner".to_string(), "Latitude".to_string()]
        );
        assert_eq!(group(&groups, "Hetzner").validator_count, 2);
    }

    #[test]
    fn unknown_placeholders_collapse_into_one_unclassified_bucket() {
        // 65535 is absent from client-ids.csv.
        let unregistered = Some(65535);
        let validators = validators(vec![
            Member::new("blank", last_two_epochs(100, None, Some("   "))),
            Member::new(
                "unknown",
                last_two_epochs(100, unregistered, Some("Unknown")),
            ),
            // `Unknown(999)`: what an RPC renders for an id absent from its table; 999 is absent
            // from client-ids.csv too.
            Member {
                client_id_raw: Some("Unknown(999)"),
                ..Member::new("placeholder", last_two_epochs(100, None, Some("unknown")))
            },
        ]);

        for kind in [GroupKind::ClientLabel, GroupKind::ProviderAso] {
            let groups = aggregate_groups(&validators, kind);
            assert_eq!(keys(&groups), vec![UNKNOWN_GROUP.to_string()], "{kind:?}");
            assert_eq!(group(&groups, UNKNOWN_GROUP).validator_count, 3, "{kind:?}");
        }
    }

    #[test]
    fn a_node_not_yet_crawled_this_epoch_keeps_the_client_it_last_reported() {
        let validators = validators(vec![Member::new(
            "uncrawled",
            vec![
                (CURRENT_EPOCH, 100, None, None),
                (PREVIOUS_EPOCH, 100, JITO_BAM, None),
            ],
        )]);

        assert_eq!(
            keys(&aggregate_groups(&validators, GroupKind::ClientLabel)),
            vec!["Agave + JitoBAM".to_string()],
            "gossip lags the epoch boundary; a missing observation is not a client change"
        );
        assert_eq!(
            keys(&aggregate_groups(&validators, GroupKind::ClientLineage)),
            vec!["Agave".to_string()]
        );
    }

    #[test]
    fn the_client_observed_this_epoch_wins_over_an_older_one() {
        let validators = validators(vec![Member::new(
            "switched",
            vec![
                (CURRENT_EPOCH, 100, FRANKENDANCER, None),
                (PREVIOUS_EPOCH, 100, JITO_BAM, None),
            ],
        )]);

        assert_eq!(
            keys(&aggregate_groups(&validators, GroupKind::ClientLabel)),
            vec!["Frankendancer".to_string()]
        );
    }

    #[test]
    fn an_unregistered_client_reported_this_epoch_stays_unclassified() {
        // What `/clients` serves for a node reporting a gossip id absent from client-ids.csv.
        let validators = validators(vec![Member {
            client_id_raw: Some("Unknown(11040)"),
            ..Member::new(
                "unregistered",
                vec![
                    (CURRENT_EPOCH, 100, None, None),
                    (PREVIOUS_EPOCH, 100, AGAVE, None),
                ],
            )
        }]);

        assert_eq!(
            keys(&aggregate_groups(&validators, GroupKind::ClientLabel)),
            vec![UNKNOWN_GROUP.to_string()],
            "the node did report, with an id the registry does not know: not a missing observation"
        );
    }

    #[test]
    fn a_client_the_registry_does_not_know_keeps_the_name_the_node_reported() {
        // A name absent from client-ids.csv.
        let validators = validators(vec![Member {
            client_id_raw: Some("Sonic"),
            ..Member::new("reported", last_two_epochs(100, None, None))
        }]);

        assert_eq!(
            keys(&aggregate_groups(&validators, GroupKind::ClientLabel)),
            vec!["Sonic".to_string()],
            "a readable name is a real client, not an unclassified one"
        );
        assert_eq!(
            keys(&aggregate_groups(&validators, GroupKind::ClientLineage)),
            vec![UNKNOWN_GROUP.to_string()],
            "which client it is built from is genuinely unknown, so it lands under the unclassified parent"
        );
    }

    #[test]
    fn net_apy_is_stake_weighted_over_every_member() {
        let validators = validators(vec![
            Member {
                net_apy: Some(0.10),
                ..Member::new("paying", last_two_epochs(100, AGAVE, None))
            },
            Member {
                net_apy: Some(0.0),
                ..Member::new("zero", last_two_epochs(900, AGAVE, None))
            },
        ]);

        let groups = aggregate_groups(&validators, GroupKind::ClientLabel);
        let net_apy = group(&groups, "Agave").net_apy.unwrap();
        assert!((net_apy - 0.01).abs() < 1e-12, "{net_apy}");
    }

    #[test]
    fn a_member_with_no_rate_does_not_dilute_the_average() {
        let validators = validators(vec![
            Member {
                net_apy: Some(0.07),
                ..Member::new("reporting", last_two_epochs(100, AGAVE, None))
            },
            Member::new("silent", last_two_epochs(900, AGAVE, None)),
        ]);

        let groups = aggregate_groups(&validators, GroupKind::ClientLabel);
        let net_apy = group(&groups, "Agave").net_apy.unwrap();
        assert!(
            (net_apy - 0.07).abs() < 1e-12,
            "stake with no rate must not dilute the rate, got {net_apy}"
        );
    }

    #[test]
    fn take_rate_is_stake_weighted_and_counts_full_takers() {
        let validators = validators(vec![
            Member {
                take_rate: Some(0.10),
                ..Member::new("sharing", last_two_epochs(500, AGAVE, None))
            },
            Member {
                take_rate: Some(1.0),
                ..Member::new("full", last_two_epochs(500, AGAVE, None))
            },
        ]);

        let take_rate = group(
            &aggregate_groups(&validators, GroupKind::ClientLabel),
            "Agave",
        )
        .take_rate
        .unwrap();
        assert!((take_rate - 0.55).abs() < 1e-12, "{take_rate}");
    }

    #[test]
    fn delegation_relationships_sum_and_stay_none_when_nobody_reports() {
        let counted = validators(vec![
            Member {
                unique_delegators: Some(12),
                ..Member::new("one", last_two_epochs(100, AGAVE, None))
            },
            Member::new("two", last_two_epochs(100, AGAVE, None)),
        ]);
        assert_eq!(
            group(&aggregate_groups(&counted, GroupKind::ClientLabel), "Agave")
                .delegation_relationship_count,
            Some(12)
        );

        let uncounted = validators(vec![Member::new("one", last_two_epochs(100, AGAVE, None))]);
        assert_eq!(
            group(
                &aggregate_groups(&uncounted, GroupKind::ClientLabel),
                "Agave"
            )
            .delegation_relationship_count,
            None,
            "no member reporting must not read as zero"
        );
    }

    /// Epoch 100 is current, 96 is ~8 days old and 85 ~30 days old, so both windows resolve.
    fn epochs_spanning_both_windows(
        stake_now: i64,
        stake_7d_ago: i64,
        stake_30d_ago: i64,
        client: Option<u16>,
        client_then: Option<u16>,
    ) -> Vec<EpochSpec> {
        vec![
            (CURRENT_EPOCH, stake_now, client, None),
            (PREVIOUS_EPOCH, stake_now, client, None),
            (96, stake_7d_ago, client_then, None),
            (85, stake_30d_ago, client_then, None),
        ]
    }

    #[test]
    fn stake_delta_measures_growth_of_a_group_that_did_not_move() {
        let validators = validators(vec![Member::new(
            "grew",
            epochs_spanning_both_windows(300, 200, 100, AGAVE, AGAVE),
        )]);

        let groups = aggregate_groups(&validators, GroupKind::ClientLabel);
        let agave = group(&groups, "Agave");
        assert_eq!(agave.stake_delta_7d, Some(Decimal::from(100)));
        assert_eq!(agave.stake_delta_30d, Some(Decimal::from(200)));
    }

    #[test]
    fn a_client_switch_shows_on_both_sides_of_the_delta() {
        let validators = validators(vec![Member::new(
            "migrated",
            epochs_spanning_both_windows(500, 500, 500, FRANKENDANCER, AGAVE),
        )]);

        let groups = aggregate_groups(&validators, GroupKind::ClientLabel);
        assert_eq!(
            group(&groups, "Frankendancer").stake_delta_7d,
            Some(Decimal::from(500)),
            "the client it moved to gained the whole stake"
        );
        assert_eq!(
            keys(&groups),
            vec!["Frankendancer".to_string()],
            "the client it left holds nothing today, so it has no row"
        );
    }

    #[test]
    fn delta_is_none_when_history_does_not_reach_the_window() {
        // Two epochs, ~2 days apart: the 7-day window has nothing to compare against.
        let validators = validators(vec![Member::new(
            "young",
            vec![
                (CURRENT_EPOCH, 300, AGAVE, None),
                (PREVIOUS_EPOCH, 200, AGAVE, None),
            ],
        )]);

        let groups = aggregate_groups(&validators, GroupKind::ClientLabel);
        assert_eq!(group(&groups, "Agave").stake_delta_7d, None);
        assert_eq!(group(&groups, "Agave").stake_delta_30d, None);
    }

    #[test]
    fn a_validator_missing_from_the_last_epoch_is_left_out() {
        let validators = validators(vec![
            Member::new("current", last_two_epochs(100, AGAVE, None)),
            Member::new("gone", vec![(PREVIOUS_EPOCH, 900, FRANKENDANCER, None)]),
        ]);

        let groups = aggregate_groups(&validators, GroupKind::ClientLabel);
        assert_eq!(keys(&groups), vec!["Agave".to_string()]);
        assert_eq!(groups.total_activated_stake, Decimal::from(100));
    }

    #[test]
    fn a_member_that_dropped_out_of_the_eligible_set_shows_as_a_loss() {
        let validators = validators(vec![
            Member::new(
                "stayed",
                epochs_spanning_both_windows(300, 300, 300, AGAVE, AGAVE),
            ),
            // Stopped voting and unstaked after epoch 96, so `/validators` no longer serves it.
            Member::new(
                "left",
                vec![
                    (CURRENT_EPOCH, 0, AGAVE, None),
                    (PREVIOUS_EPOCH, 0, AGAVE, None),
                    (96, 700, AGAVE, None),
                    (85, 700, AGAVE, None),
                ],
            ),
        ]);

        let groups = aggregate_groups(&validators, GroupKind::ClientLabel);
        let agave = group(&groups, "Agave");
        assert_eq!(agave.validator_count, 1);
        assert_eq!(agave.total_stake, Decimal::from(300));
        assert_eq!(agave.stake_delta_7d, Some(Decimal::from(-700)));
        assert_eq!(agave.stake_delta_30d, Some(Decimal::from(-700)));
    }

    #[test]
    fn provider_names_differing_only_in_case_are_one_group() {
        let validators = validators(vec![
            Member::new("shouty", last_two_epochs(300, AGAVE, Some("RETN Limited"))),
            Member::new("titled", last_two_epochs(100, AGAVE, Some("Retn Limited"))),
        ]);

        let groups = aggregate_groups(&validators, GroupKind::ProviderAso);
        assert_eq!(
            keys(&groups),
            vec!["RETN Limited".to_string()],
            "the spelling most stake reports is the one served"
        );
        assert_eq!(group(&groups, "RETN Limited").validator_count, 2);
        assert_eq!(
            group(&groups, "RETN Limited").total_stake,
            Decimal::from(400)
        );
    }

    #[test]
    fn no_delta_against_an_epoch_that_classified_nothing() {
        // Client ids reach back only to the epoch collection started.
        let validators = validators(vec![Member::new(
            "backfilled",
            vec![
                (CURRENT_EPOCH, 500, AGAVE, Some("Hetzner")),
                (PREVIOUS_EPOCH, 500, AGAVE, Some("Hetzner")),
                (96, 500, None, Some("Hetzner")),
                (85, 500, None, Some("Hetzner")),
            ],
        )]);

        let clients = aggregate_groups(&validators, GroupKind::ClientLabel);
        assert_eq!(group(&clients, "Agave").stake_delta_7d, None);
        assert_eq!(group(&clients, "Agave").stake_delta_30d, None);

        let providers = aggregate_groups(&validators, GroupKind::ProviderAso);
        assert_eq!(
            group(&providers, "Hetzner").stake_delta_7d,
            Some(Decimal::ZERO)
        );
    }

    #[test]
    fn a_validator_the_list_does_not_serve_counts_nowhere() {
        // Present in the last epoch but neither voting nor staked: `/validators` drops it.
        let idle = Member::new("idle", last_two_epochs(0, FRANKENDANCER, Some("Latitude")));
        let validators = validators(vec![
            Member::new("live", last_two_epochs(700, AGAVE, Some("Hetzner"))),
            idle,
        ]);

        let all = aggregate_all(&validators);
        assert_eq!(
            all.clients
                .nodes
                .iter()
                .map(|node| node.group.key.clone())
                .collect::<Vec<_>>(),
            vec!["Agave".to_string()],
            "the idle validator's client has no live stake, so it has no row"
        );
        assert_eq!(keys(&all.providers), vec!["Hetzner".to_string()]);
        assert_eq!(all.providers.total_activated_stake, Decimal::from(700));
    }

    #[test]
    fn a_validator_with_no_stake_but_credits_still_counts() {
        // `/validators` keeps a voting validator with no stake.
        let mut validators = validators(vec![
            Member::new("voting", last_two_epochs(0, AGAVE, Some("Hetzner"))),
            Member::new("staked", last_two_epochs(700, AGAVE, Some("Hetzner"))),
        ]);
        for stats in validators.get_mut("voting").unwrap().epoch_stats.iter_mut() {
            stats.credits = 1;
        }

        assert_eq!(
            group(
                &aggregate_groups(&validators, GroupKind::ProviderAso),
                "Hetzner"
            )
            .validator_count,
            2
        );
    }

    #[test]
    fn rows_open_ordered_by_stake_with_the_key_as_tiebreak() {
        let validators = validators(vec![
            Member::new("small", last_two_epochs(100, JITO_BAM, None)),
            Member::new("big", last_two_epochs(900, AGAVE, None)),
            Member::new("tied", last_two_epochs(100, FRANKENDANCER, None)),
        ]);

        let groups = aggregate_groups(&validators, GroupKind::ClientLabel);
        let ordered = keys(&groups);
        assert_eq!(ordered[0], "Agave".to_string());
        assert_eq!(
            ordered[1..],
            ["Agave + JitoBAM".to_string(), "Frankendancer".to_string()],
            "equal stake falls back to the key so paging stays stable"
        );
    }

    #[test]
    fn aggregate_all_agrees_with_aggregating_one_kind() {
        let validators = validators(vec![
            Member::new(
                "one",
                epochs_spanning_both_windows(300, 200, 100, AGAVE, AGAVE),
            ),
            Member::new(
                "two",
                epochs_spanning_both_windows(500, 500, 500, FRANKENDANCER, AGAVE),
            ),
        ]);

        let all = aggregate_all(&validators);

        let providers = aggregate_groups(&validators, GroupKind::ProviderAso);
        assert_eq!(keys(&providers), keys(&all.providers));
        assert_eq!(providers.current_epoch, all.providers.current_epoch);

        let clients = aggregate_groups(&validators, GroupKind::ClientLineage);
        assert_eq!(
            clients
                .groups
                .iter()
                .map(|group| group.key.clone())
                .collect::<Vec<_>>(),
            all.clients
                .nodes
                .iter()
                .map(|node| node.group.key.clone())
                .collect::<Vec<_>>(),
            "the tree's parents are the client grouping"
        );
        assert_eq!(
            clients.groups.first().map(|group| group.stake_delta_7d),
            all.clients
                .nodes
                .first()
                .map(|node| node.group.stake_delta_7d)
        );
        assert_eq!(clients.current_epoch, all.clients.current_epoch);
    }

    #[test]
    fn epoch_resolution_picks_the_newest_epoch_that_had_ended_by_the_cutoff() {
        let validators = validators(vec![Member::new(
            "one",
            vec![
                (CURRENT_EPOCH, 100, AGAVE, None),
                (PREVIOUS_EPOCH, 100, AGAVE, None),
                (96, 100, AGAVE, None),
                (95, 100, AGAVE, None),
                (85, 100, AGAVE, None),
            ],
        )]);

        assert_eq!(Population::new(&validators).unwrap().current_epoch, 100);
        // Epochs run ~2 days: 96 ended ~8 days ago, 95 ~10, so 96 is the newest one old enough.
        assert_eq!(delta_epochs(validators.values()), (Some(96), Some(85)));
    }

    fn tree(validators: &HashMap<String, ValidatorRecord>) -> ValidatorGroupTree {
        aggregate_all(validators).clients
    }

    fn child_keys(node: &ValidatorGroupNode) -> Vec<String> {
        node.children
            .iter()
            .map(|child| child.key.clone())
            .collect()
    }

    #[test]
    fn block_engines_sit_under_the_client_they_run_with() {
        let validators = validators(vec![
            Member::new("plain", last_two_epochs(100, AGAVE, None)),
            Member::new("jito", last_two_epochs(400, Some(1), None)),
            Member::new("bam", last_two_epochs(200, JITO_BAM, None)),
            Member::new("frank", last_two_epochs(300, FRANKENDANCER, None)),
            Member::new("fire", last_two_epochs(50, Some(5), None)),
        ]);

        let tree = tree(&validators);
        assert_eq!(
            tree.nodes
                .iter()
                .map(|node| node.group.key.clone())
                .collect::<Vec<_>>(),
            vec![
                "Agave".to_string(),
                "Frankendancer".to_string(),
                "Firedancer".to_string()
            ],
            "parents open ordered by stake"
        );

        let agave = &tree.nodes[0];
        assert_eq!(agave.group.validator_count, 3);
        assert_eq!(agave.group.total_stake, Decimal::from(700));
        assert_eq!(
            child_keys(agave),
            vec![
                "Agave + Jito".to_string(),
                "Agave + JitoBAM".to_string(),
                "Agave".to_string()
            ],
            "a client running no separate block engine keys as the client itself"
        );
        assert_eq!(
            child_keys(&tree.nodes[1]),
            vec!["Frankendancer".to_string()]
        );
    }

    #[test]
    fn a_client_is_aggregated_over_its_own_validators_not_summed_from_its_block_engines() {
        // 0.10 on 900 stake and 0.02 on 100 weights to 0.092, not the 0.06 plain mean.
        let validators = validators(vec![
            Member {
                net_apy: Some(0.10),
                ..Member::new("big", last_two_epochs(900, AGAVE, None))
            },
            Member {
                net_apy: Some(0.02),
                ..Member::new("small", last_two_epochs(100, Some(1), None))
            },
        ]);

        let tree = tree(&validators);
        let agave = &tree.nodes[0];
        let net_apy = agave.group.net_apy.unwrap();
        assert!(
            (net_apy - 0.092).abs() < 1e-12,
            "expected the stake-weighted rate, got {net_apy}"
        );
        assert_eq!(child_keys(agave).len(), 2);
    }

    #[test]
    fn a_block_engine_the_registry_cannot_place_lands_under_the_unclassified_parent() {
        let validators = validators(vec![
            Member::new("known", last_two_epochs(900, AGAVE, None)),
            Member {
                client_id_raw: Some("Sonic"),
                ..Member::new("unplaceable", last_two_epochs(100, None, None))
            },
        ]);

        let tree = tree(&validators);
        let unclassified = tree
            .nodes
            .iter()
            .find(|node| node.group.key == UNKNOWN_GROUP)
            .expect("the unclassified parent has to be served, or its stake vanishes");
        assert_eq!(
            child_keys(unclassified),
            vec!["Sonic".to_string()],
            "the row keeps the name the node reported even though its client is unknown"
        );
    }

    #[test]
    fn parent_shares_sum_to_one_across_the_tree() {
        let validators = validators(vec![
            Member::new("agave", last_two_epochs(700, AGAVE, None)),
            Member::new("frank", last_two_epochs(200, FRANKENDANCER, None)),
            Member::new("unknown", last_two_epochs(100, None, None)),
        ]);

        let tree = tree(&validators);
        let total: f64 = tree.nodes.iter().map(|node| node.group.stake_share).sum();
        assert!((total - 1.0).abs() < 1e-12, "{total}");
        assert_eq!(tree.total_activated_stake, Decimal::from(1000));
    }

    #[test]
    fn a_block_engine_switch_inside_one_client_leaves_the_parent_delta_flat() {
        // Agave + Jito -> Agave + JitoBAM: same client, different block engine.
        let validators = validators(vec![Member::new(
            "switched",
            epochs_spanning_both_windows(500, 500, 500, JITO_BAM, Some(1)),
        )]);

        let tree = tree(&validators);
        let agave = &tree.nodes[0];
        assert_eq!(agave.group.key, "Agave".to_string());
        assert_eq!(
            agave.group.stake_delta_7d,
            Some(Decimal::ZERO),
            "the client neither gained nor lost stake"
        );
        assert_eq!(
            agave.children[0].stake_delta_7d,
            Some(Decimal::from(500)),
            "the block engine it moved to gained all of it"
        );
    }

    #[test]
    fn operators_group_the_vote_accounts_the_mapping_names() {
        let validators = validators(vec![
            Member::new(FIGMENT_ONE, last_two_epochs(300, AGAVE, None)),
            Member::new(FIGMENT_TWO, last_two_epochs(200, FRANKENDANCER, None)),
            Member::new(HELIUS, last_two_epochs(100, AGAVE, None)),
        ]);

        let groups = operators(&validators);
        assert_eq!(
            keys(&groups),
            vec!["Figment".to_string(), "Helius".to_string()]
        );

        let figment = group(&groups, "Figment");
        assert_eq!(figment.validator_count, 2);
        assert_eq!(figment.total_stake, Decimal::from(500));
        assert!((figment.stake_share - 500.0 / 600.0).abs() < 1e-12);
    }

    #[test]
    fn operator_rows_describe_only_the_validators_they_are_aggregated_over() {
        let validators = validators(vec![
            Member::new(FIGMENT_ONE, last_two_epochs(300, AGAVE, None)),
            Member::new(FIGMENT_TWO, last_two_epochs(200, FRANKENDANCER, None)),
            Member::new(HELIUS, last_two_epochs(900, AGAVE, None)),
        ]);
        let whole: Vec<&ValidatorRecord> = validators.values().collect();
        let one_figment: Vec<&ValidatorRecord> = validators
            .values()
            .filter(|validator| validator.vote_account == FIGMENT_ONE)
            .collect();

        let all = aggregate_operators(&whole);
        assert_eq!(
            keys(&all),
            vec!["Helius".to_string(), "Figment".to_string()]
        );
        assert_eq!(group(&all, "Figment").validator_count, 2);
        assert_eq!(group(&all, "Figment").total_stake, Decimal::from(500));

        let filtered = aggregate_operators(&one_figment);
        assert_eq!(
            keys(&filtered),
            vec!["Figment".to_string()],
            "an operator none of these validators belongs to has no row"
        );
        let figment = group(&filtered, "Figment");
        assert_eq!(figment.validator_count, 1);
        assert_eq!(figment.total_stake, Decimal::from(300));
        assert!(
            (figment.stake_share - 1.0).abs() < 1e-12,
            "the share is of the validators aggregated over, got {}",
            figment.stake_share
        );
    }

    #[test]
    fn an_unmapped_validator_makes_no_row_but_still_counts_towards_the_shares() {
        let validators = validators(vec![
            Member::new(FIGMENT_ONE, last_two_epochs(300, AGAVE, None)),
            Member::new("nobody", last_two_epochs(700, AGAVE, None)),
        ]);

        let groups = operators(&validators);
        assert_eq!(
            keys(&groups),
            vec!["Figment".to_string()],
            "an unmapped validator belongs to no operator, so it gets no bucket of its own"
        );
        assert_eq!(
            groups.total_activated_stake,
            Decimal::from(1000),
            "the denominator describes the cluster, not the mapped subset"
        );
        assert!((group(&groups, "Figment").stake_share - 0.3).abs() < 1e-12);
    }

    #[test]
    fn the_weighted_columns_follow_the_stake_behind_each_member() {
        let validators = validators(vec![
            Member {
                credits: 100,
                marinade_score: Some(0.2),
                apy: Some(0.05),
                commission_max_observed: Some(10),
                uptime_pct: Some(90.0),
                expected_take_rate: Some(0.04),
                ..Member::new(FIGMENT_ONE, last_two_epochs(300, AGAVE, None))
            },
            Member {
                credits: 200,
                marinade_score: Some(0.4),
                apy: Some(0.07),
                commission_advertised: Some(5),
                uptime_pct: Some(100.0),
                expected_take_rate: Some(0.08),
                ..Member::new(FIGMENT_TWO, last_two_epochs(100, AGAVE, None))
            },
        ]);

        let figment = &operators(&validators);
        let figment = group(figment, "Figment");
        let weighted = |low: f64, high: f64| (3.0 * low + high) / 4.0;

        for (column, value, expected) in [
            ("credits", figment.credits, weighted(100.0, 200.0)),
            ("marinade_score", figment.marinade_score, weighted(0.2, 0.4)),
            ("apy", figment.apy, weighted(0.05, 0.07)),
            ("commission", figment.commission, weighted(10.0, 5.0)),
            ("uptime_pct", figment.uptime_pct, weighted(90.0, 100.0)),
            (
                "expected_take_rate",
                figment.expected_take_rate,
                weighted(0.04, 0.08),
            ),
        ] {
            let value = value.unwrap_or_else(|| panic!("{column} is missing"));
            assert!(
                (value - expected).abs() < 1e-12,
                "{column} reads {value}, not {expected}"
            );
        }
    }

    #[test]
    fn a_lone_validator_reads_the_same_columns_as_a_group_holding_only_it() {
        let validators = validators(vec![Member {
            credits: 100,
            marinade_score: Some(0.2),
            apy: Some(0.05),
            net_apy: Some(0.06),
            take_rate: Some(0.03),
            commission_max_observed: Some(10),
            commission_advertised: Some(5),
            uptime_pct: Some(0.99),
            expected_take_rate: Some(0.04),
            unique_delegators: Some(12),
            incidents_days_ago: vec![1],
            ..Member::new(FIGMENT_ONE, last_two_epochs(300, AGAVE, None))
        }]);
        let figment = group(&operators(&validators), "Figment").clone();
        let figment_incidents = figment.incidents.count();

        let validator = ValidatorRecord {
            activated_stake: Decimal::from(300),
            ..validators.get(FIGMENT_ONE).unwrap().clone()
        };
        assert_eq!(
            singleton_group(&validator),
            ValidatorGroupRecord {
                key: FIGMENT_ONE.to_string(),
                stake_share: 0.0,
                incidents: GroupIncidents::Count(figment_incidents),
                ..figment
            },
            "the two are ordered against each other, so every column has to agree"
        );
        assert_eq!(figment_incidents, 1);
    }

    #[test]
    fn a_member_with_no_value_of_its_own_leaves_the_weighted_column_alone() {
        let validators = validators(vec![
            Member {
                uptime_pct: Some(90.0),
                ..Member::new(FIGMENT_ONE, last_two_epochs(100, AGAVE, None))
            },
            Member::new(FIGMENT_TWO, last_two_epochs(900, AGAVE, None)),
        ]);

        let figment = &operators(&validators);
        let figment = group(figment, "Figment");
        assert_eq!(
            figment.uptime_pct,
            Some(90.0),
            "the silent member neither dilutes the mean nor reads as zero"
        );
        assert_eq!(
            figment.commission, None,
            "a group whose members all have an unknown commission has none of its own"
        );
    }

    #[test]
    fn the_weighted_commission_takes_the_worse_side_each_member_reports() {
        let validators = validators(vec![Member {
            commission_max_observed: Some(100),
            commission_advertised: Some(5),
            ..Member::new(FIGMENT_ONE, last_two_epochs(100, AGAVE, None))
        }]);

        assert_eq!(
            group(&operators(&validators), "Figment").commission,
            Some(100.0),
            "an observed ceiling beats a lower advertised rate"
        );
    }

    #[test]
    fn operator_stake_delta_measures_the_whole_group() {
        let validators = validators(vec![
            Member::new(
                FIGMENT_ONE,
                vec![
                    (CURRENT_EPOCH, 300, AGAVE, None),
                    (PREVIOUS_EPOCH, 300, AGAVE, None),
                    (CURRENT_EPOCH - 5, 100, AGAVE, None),
                ],
            ),
            Member::new(
                FIGMENT_TWO,
                vec![
                    (CURRENT_EPOCH, 200, AGAVE, None),
                    (PREVIOUS_EPOCH, 200, AGAVE, None),
                    (CURRENT_EPOCH - 5, 200, AGAVE, None),
                ],
            ),
        ]);

        let figment = &operators(&validators);
        let figment = group(figment, "Figment");
        assert_eq!(figment.stake_delta_7d, Some(Decimal::from(200)));
    }

    #[test]
    fn a_group_carries_every_member_incident_oldest_first() {
        let validators = validators(vec![
            Member {
                incidents_days_ago: vec![1, 89],
                ..Member::new(FIGMENT_ONE, last_two_epochs(300, AGAVE, None))
            },
            Member {
                incidents_days_ago: vec![30, 200],
                ..Member::new(FIGMENT_TWO, last_two_epochs(200, AGAVE, None))
            },
        ]);

        let figment = operators(&validators);
        let GroupIncidents::Records(incidents) = &group(&figment, "Figment").incidents else {
            panic!("operator rows carry the records themselves");
        };
        assert_eq!(incidents.len(), 4);
        assert!(incidents
            .windows(2)
            .all(|pair| pair[0].detail.started_at() <= pair[1].detail.started_at()));
        assert_eq!(
            incidents
                .iter()
                .map(|incident| incident.validator.as_str())
                .collect::<Vec<_>>(),
            vec![FIGMENT_TWO, FIGMENT_ONE, FIGMENT_TWO, FIGMENT_ONE],
            "each incident names the member that was down"
        );
    }

    #[test]
    fn only_operator_rows_carry_the_records_the_rest_carry_the_count() {
        let validators = validators(vec![Member {
            incidents_days_ago: vec![1, 2],
            ..Member::new(FIGMENT_ONE, last_two_epochs(300, AGAVE, None))
        }]);

        for kind in [
            GroupKind::ClientLineage,
            GroupKind::ClientLabel,
            GroupKind::ProviderAso,
        ] {
            let groups = aggregate_groups(&validators, kind);
            assert!(
                groups
                    .groups
                    .iter()
                    .all(|group| matches!(group.incidents, GroupIncidents::Count(_))),
                "{kind:?} rows span most of the cluster"
            );
            assert_eq!(
                groups
                    .groups
                    .iter()
                    .map(|group| group.incidents.count())
                    .sum::<u64>(),
                2,
                "{kind:?} counts the same downtime the operator row lists"
            );
        }
    }

    #[test]
    fn a_group_with_no_incidents_reports_an_empty_array_rather_than_being_left_out() {
        let validators = validators(vec![Member::new(
            FIGMENT_ONE,
            last_two_epochs(300, AGAVE, None),
        )]);

        assert_eq!(
            group(&operators(&validators), "Figment").incidents,
            GroupIncidents::Records(Vec::new())
        );
    }
}
