use crate::dto::{
    client_label, client_lineage, effective_client_id, ValidatorEpochStats, ValidatorGroupNode,
    ValidatorGroupRecord, ValidatorGroupTree, ValidatorGroups, ValidatorRecord,
};
use crate::utils::{is_eligible_validator, last_reported_epoch};
use chrono::{DateTime, Duration, Utc};
use rust_decimal::prelude::*;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

/// Windows the stake deltas describe. Epochs run ~2 days, so a delta is epoch-granular; the epoch
/// each window resolved to is served next to the values so a consumer can say what it compared.
pub const DELTA_SHORT_DAYS: i64 = 7;
pub const DELTA_LONG_DAYS: i64 = 30;

/// A net APY below this renders as `0.00%` at the two decimals consumers show. apy-api derives the
/// series in floating point, so a validator paying its stakers nothing reports dust rather than an
/// exact zero — testing `== 0.0` would weight that stake into a rate the group does not pay.
const NEGLIGIBLE_NET_APY: f64 = 0.00005;

/// A take rate this high renders as `100.00%`. Full takers report a hair under 1 rather than
/// exactly 1, so `>= 1.0` would leave stake a staker cannot earn on inside the average.
const FULL_TAKE_RATE: f64 = 0.99995;

/// Which field of a validator names the group it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupKind {
    /// The client variant a validator runs: the client plus its vendor's modification, `Agave + Jito`.
    ClientLabel,
    /// The client a variant is built from, `Agave`. The parent level of the client tree.
    ClientLineage,
    /// The hosting organisation, `Hetzner`.
    ProviderAso,
}

/// Comparison ignores case so `Hetzner` and `hetzner` cannot straddle a page boundary, then falls back
/// to the exact spelling so the order is total.
pub fn compare_keys(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase()).then(a.cmp(b))
}

/// Served as the key of the bucket holding validators whose value is unknown. The same word the API
/// already serves for a client it cannot name, so a consumer renders one thing either way, and the
/// bucket is a row like any other rather than a null every caller has to special-case.
pub const UNKNOWN_GROUP: &str = "Unknown";

/// `None` for a value that names no group: empty, or one of the diagnostic placeholders an RPC
/// renders for a client it cannot identify (`Unknown`, `Unknown(8)`). Those are not a group anyone
/// ships, so they collapse into the one unclassified bucket instead of several fake ones.
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

/// The client the node reported, for one the Solana Foundation registry does not know. Kept as its
/// own group rather than folded into the unclassified bucket: a readable name is a real client that
/// simply landed before our registry row did, and lumping it in hides stake behind `null`.
fn reported_client(stats: &ValidatorEpochStats) -> Option<String> {
    normalized(stats.client_id_raw.clone())
}

/// The registry renders a client lowercase (`agave`), which reads wrong next to the variant labels it
/// parents (`Agave + Jito`), so the parent row is served title-cased.
fn as_lineage_name(lineage: String) -> String {
    let mut characters = lineage.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => lineage,
    }
}

fn group_key(stats: &ValidatorEpochStats, kind: GroupKind) -> Option<String> {
    // Re-resolved per epoch, so a group's key is derived the same way the record's own client
    // fields are and the two can never disagree.
    let client_id = effective_client_id(stats.client_id, stats.client_id_raw.as_deref());

    match kind {
        GroupKind::ProviderAso => normalized(stats.dc_aso.clone()),
        GroupKind::ClientLabel => {
            normalized(Some(client_label(client_id))).or_else(|| reported_client(stats))
        }
        // No raw fallback: which client a variant the registry cannot name is built from is unknown,
        // not whatever string the node happened to report. Such a variant still gets its own child row
        // and lands under the unclassified parent.
        GroupKind::ClientLineage => normalized(client_lineage(client_id)).map(as_lineage_name),
    }
}

/// Case-folded group identity. The IP-geolocation source re-cases provider names between epochs
/// (`RETN Limited` and `Retn Limited` are the same company, and nine such pairs appear in a single
/// page of validators today), so folding is what stops one provider becoming two rows and its stake
/// reading as a migration that never happened.
type FoldedKey = Option<String>;

fn folded(key: &Option<String>) -> FoldedKey {
    key.as_ref().map(|key| key.to_lowercase())
}

#[derive(Default)]
struct Accumulator {
    /// Spellings seen for this group and the stake behind each, so the row can be served under the
    /// one that most stake reports rather than whichever validator happened to be visited first.
    spellings: HashMap<String, Decimal>,
    validator_count: u64,
    total_stake: Decimal,
    net_apy_weighted: f64,
    net_apy_weight: f64,
    take_rate_weighted: f64,
    take_rate_weight: f64,
    delegator_count: Option<u64>,
}

impl Accumulator {
    fn key(&self) -> Option<String> {
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
        key: Option<&String>,
    ) {
        self.validator_count += 1;
        self.total_stake += stats.activated_stake;

        if let Some(key) = key {
            *self.spellings.entry(key.clone()).or_default() += stats.activated_stake;
        }

        // Rates are stake-weighted so a provider's handful of dust validators cannot swing a figure
        // describing millions of staked SOL. A validator without a usable rate leaves both sides of
        // the ratio, so it neither dilutes the rate nor lends it stake.
        let weight = stats.activated_stake.to_f64().unwrap_or_default();

        if let Some(net_apy) = validator
            .net_apy
            .filter(|apy| apy.is_finite() && *apy >= NEGLIGIBLE_NET_APY)
        {
            self.net_apy_weighted += net_apy * weight;
            self.net_apy_weight += weight;
        }

        if let Some(take_rate) = validator
            .avg_take_rate
            .filter(|rate| rate.is_finite() && *rate >= 0.0 && *rate < FULL_TAKE_RATE)
        {
            self.take_rate_weighted += take_rate * weight;
            self.take_rate_weight += weight;
        }

        // `None` rather than 0 when nobody in the group reports a count, so a consumer renders the
        // missing-data placeholder instead of claiming the group has no delegators.
        if let Some(unique_delegators) = validator.unique_delegators {
            self.delegator_count =
                Some(self.delegator_count.unwrap_or_default() + unique_delegators);
        }
    }

    fn finish(
        self,
        folded_key: &FoldedKey,
        total_activated_stake: Decimal,
        stake_short: Option<&ReferenceStake>,
        stake_long: Option<&ReferenceStake>,
    ) -> ValidatorGroupRecord {
        let delta = |reference: Option<&ReferenceStake>| {
            reference.map(|stake_by_key| {
                self.total_stake - stake_by_key.get(folded_key).copied().unwrap_or_default()
            })
        };

        ValidatorGroupRecord {
            key: self.key().unwrap_or_else(|| UNKNOWN_GROUP.to_string()),
            validator_count: self.validator_count,
            total_stake: self.total_stake,
            stake_share: if total_activated_stake.is_zero() {
                0.0
            } else {
                (self.total_stake / total_activated_stake)
                    .to_f64()
                    .unwrap_or_default()
            },
            stake_delta_7d: delta(stake_short),
            stake_delta_30d: delta(stake_long),
            net_apy: weighted(self.net_apy_weighted, self.net_apy_weight),
            take_rate: weighted(self.take_rate_weighted, self.take_rate_weight),
            delegator_count: self.delegator_count,
        }
    }
}

fn weighted(weighted_sum: f64, weight: f64) -> Option<f64> {
    (weight > 0.0).then(|| weighted_sum / weight)
}

/// Which epochs the aggregation reads, resolved once: none of it depends on how validators are
/// grouped, so every grouping shares one walk of the epoch stats rather than repeating three.
pub struct Epochs {
    current: u64,
    delta_7d: Option<u64>,
    delta_30d: Option<u64>,
}

fn epochs(validators: &[&ValidatorRecord], now: DateTime<Utc>) -> Option<Epochs> {
    let mut current = None;
    let mut ends: HashMap<u64, DateTime<Utc>> = Default::default();

    for stats in validators
        .iter()
        .flat_map(|validator| &validator.epoch_stats)
    {
        current = current.max(Some(stats.epoch));
        if let Some(epoch_end_at) = stats.epoch_end_at {
            ends.insert(stats.epoch, epoch_end_at);
        }
    }

    Some(Epochs {
        current: current?,
        delta_7d: reference_epoch(&ends, now, DELTA_SHORT_DAYS),
        delta_30d: reference_epoch(&ends, now, DELTA_LONG_DAYS),
    })
}

/// Newest epoch that had already ended `days` ago, or `None` when history does not reach that far
/// back — a delta against the oldest epoch we happen to hold would describe an unknown window.
fn reference_epoch(
    epoch_ends: &HashMap<u64, DateTime<Utc>>,
    now: DateTime<Utc>,
    days: i64,
) -> Option<u64> {
    let cutoff = now - Duration::days(days);
    epoch_ends
        .iter()
        .filter(|(_, epoch_end_at)| **epoch_end_at <= cutoff)
        .map(|(epoch, _)| *epoch)
        .max()
}

type ReferenceStake = HashMap<FoldedKey, Decimal>;

/// Stake per group as it stood in `epoch`, bucketed by that epoch's own client and provider values.
/// This is what makes a delta describe adoption rather than membership: a validator that moved from
/// Agave to Frankendancer counted towards Agave then and towards Frankendancer now, so the move
/// shows as a loss on one and a gain on the other.
///
/// `None` when that epoch classified nothing — client ids only reach back to the epoch collection of
/// them started, and against an epoch where every validator was unclassified every group would read
/// as having appeared from nothing. Self-heals as history accumulates.
fn stake_by_key_at(
    validators: &[&ValidatorRecord],
    epoch: u64,
    kind: GroupKind,
) -> Option<ReferenceStake> {
    let mut stake_by_key: ReferenceStake = Default::default();
    for validator in validators {
        if let Some(stats) = validator.epoch_stats.iter().find(|s| s.epoch == epoch) {
            *stake_by_key
                .entry(folded(&group_key(stats, kind)))
                .or_default() += stats.activated_stake;
        }
    }

    stake_by_key
        .keys()
        .any(|key| key.is_some())
        .then_some(stake_by_key)
}

/// One row per client or per hosting provider, aggregated over the whole validator set as it stands
/// in the last epoch. Rows come from that epoch alone, so a group nobody runs today has no row even
/// if it held stake a month ago.
pub fn aggregate_groups(
    validators: &HashMap<String, ValidatorRecord>,
    kind: GroupKind,
) -> ValidatorGroups {
    let eligible = eligible(validators);
    let Some(epochs) = epochs(&eligible, Utc::now()) else {
        return Default::default();
    };

    aggregate_kind(&eligible, kind, &epochs)
}

/// The validators every group is aggregated over: the same population the validator list serves, so a
/// group's stake share is a share of a total a consumer can also read off `/validators`. Judged
/// against the newest epoch the whole cache reports, exactly as the list judges it.
fn eligible(validators: &HashMap<String, ValidatorRecord>) -> Vec<&ValidatorRecord> {
    let last_epoch = last_reported_epoch(validators.values()).unwrap_or(0);
    validators
        .values()
        .filter(|validator| is_eligible_validator(validator, last_epoch))
        .collect()
}

/// Rows with the folded key each was bucketed under, which is what joins the two levels of the client
/// tree — the rendered key cannot do it, since two spellings and the unknown bucket all normalise.
struct KeyedGroups {
    rows: Vec<(FoldedKey, ValidatorGroupRecord)>,
    groups: ValidatorGroups,
}

fn aggregate_kind(
    validators: &[&ValidatorRecord],
    kind: GroupKind,
    epochs: &Epochs,
) -> ValidatorGroups {
    aggregate_keyed(validators, kind, epochs).groups
}

fn aggregate_keyed(
    validators: &[&ValidatorRecord],
    kind: GroupKind,
    epochs: &Epochs,
) -> KeyedGroups {
    let current_epoch = epochs.current;
    let stake_short = epochs
        .delta_7d
        .and_then(|epoch| stake_by_key_at(validators, epoch, kind));
    let stake_long = epochs
        .delta_30d
        .and_then(|epoch| stake_by_key_at(validators, epoch, kind));

    let mut accumulators: HashMap<FoldedKey, Accumulator> = Default::default();
    for validator in validators {
        let Some(stats) = validator
            .epoch_stats
            .iter()
            .find(|stats| stats.epoch == current_epoch)
        else {
            continue;
        };

        let key = group_key(stats, kind);
        accumulators
            .entry(folded(&key))
            .or_default()
            .add(validator, stats, key.as_ref());
    }

    let total_activated_stake = accumulators
        .values()
        .map(|accumulator| accumulator.total_stake)
        .sum();

    let mut rows: Vec<_> = accumulators
        .into_iter()
        .map(|(folded_key, accumulator)| {
            let record = accumulator.finish(
                &folded_key,
                total_activated_stake,
                stake_short.as_ref(),
                stake_long.as_ref(),
            );
            (folded_key, record)
        })
        .collect();

    // Largest stake first, tiebroken on the key: HashMap iteration order changes on every cache
    // refresh, and an unstable base order would make paged reads overlap or skip rows.
    rows.sort_by(|(_, a), (_, b)| {
        b.total_stake
            .cmp(&a.total_stake)
            .then_with(|| compare_keys(&a.key, &b.key))
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

/// Clients as a two-level tree: one parent per client, its variants underneath.
///
/// Both levels are aggregated over the same validators, then joined by which client each variant is
/// built from, so a parent is a genuine aggregate of the client rather than a sum of the child rows.
fn aggregate_client_tree(validators: &[&ValidatorRecord], epochs: &Epochs) -> ValidatorGroupTree {
    let lineages = aggregate_keyed(validators, GroupKind::ClientLineage, epochs);
    let variants = aggregate_keyed(validators, GroupKind::ClientLabel, epochs);
    let mut children_by_lineage = children_by_lineage(validators, epochs.current);

    let nodes = lineages
        .rows
        .into_iter()
        .map(|(folded_lineage, lineage)| {
            let children = children_by_lineage
                .remove(&folded_lineage)
                .unwrap_or_default();

            ValidatorGroupNode {
                children: variants
                    .rows
                    .iter()
                    .filter(|(folded_variant, _)| children.contains(folded_variant))
                    .map(|(_, variant)| variant.clone())
                    .collect(),
                group: lineage,
            }
        })
        .collect();

    ValidatorGroupTree {
        nodes,
        total_activated_stake: lineages.groups.total_activated_stake,
        current_epoch: lineages.groups.current_epoch,
    }
}

/// Which variant labels sit under which client, read off the validators themselves rather than from a
/// table of the registry's pairings: a variant only appears where stake actually runs it.
fn children_by_lineage(
    validators: &[&ValidatorRecord],
    current_epoch: u64,
) -> HashMap<FoldedKey, HashSet<FoldedKey>> {
    let mut children: HashMap<FoldedKey, HashSet<FoldedKey>> = Default::default();
    for validator in validators {
        if let Some(stats) = validator
            .epoch_stats
            .iter()
            .find(|stats| stats.epoch == current_epoch)
        {
            children
                .entry(folded(&group_key(stats, GroupKind::ClientLineage)))
                .or_default()
                .insert(folded(&group_key(stats, GroupKind::ClientLabel)));
        }
    }
    children
}

/// The client tree and the provider rows, sharing one epoch resolution since that part does not
/// depend on how validators are grouped.
pub fn aggregate_all(validators: &HashMap<String, ValidatorRecord>) -> ValidatorGroupings {
    let eligible = eligible(validators);
    let Some(epochs) = epochs(&eligible, Utc::now()) else {
        return Default::default();
    };

    ValidatorGroupings {
        clients: aggregate_client_tree(&eligible, &epochs),
        providers: aggregate_kind(&eligible, GroupKind::ProviderAso, &epochs),
    }
}

/// Everything the group endpoints serve, aggregated once per cache refresh.
#[derive(Default, Clone)]
pub struct ValidatorGroupings {
    pub clients: ValidatorGroupTree,
    pub providers: ValidatorGroups,
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPOCH_SECONDS: i64 = 2 * 24 * 3600;

    fn epoch_end(epoch: u64, last_epoch: u64) -> DateTime<Utc> {
        Utc::now() - Duration::seconds((last_epoch - epoch) as i64 * EPOCH_SECONDS)
    }

    /// One epoch of a fixture validator: `(epoch, stake, client_id, dc_aso)`.
    type EpochSpec = (u64, i64, Option<u16>, Option<&'static str>);

    const CURRENT_EPOCH: u64 = 100;
    const PREVIOUS_EPOCH: u64 = 99;

    /// The two epochs `is_eligible_validator` reads, both carrying the same stake and client. A
    /// fixture has to state both or the validator is not one the API describes at all.
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
        unique_delegators: Option<u64>,
        client_id_raw: Option<&'static str>,
    }

    impl Member {
        fn new(vote_account: &'static str, epochs: Vec<EpochSpec>) -> Self {
            Self {
                vote_account,
                epochs,
                net_apy: None,
                take_rate: None,
                unique_delegators: None,
                client_id_raw: None,
            }
        }
    }

    fn validators(members: Vec<Member>) -> HashMap<String, ValidatorRecord> {
        let last_epoch = members
            .iter()
            .flat_map(|member| member.epochs.iter().map(|(epoch, ..)| *epoch))
            .max()
            .unwrap_or_default();

        members
            .into_iter()
            .map(|member| {
                let epoch_stats = member
                    .epochs
                    .iter()
                    .map(|(epoch, stake, client_id, dc_aso)| ValidatorEpochStats {
                        epoch: *epoch,
                        epoch_end_at: Some(epoch_end(*epoch, last_epoch)),
                        activated_stake: Decimal::from(*stake),
                        client_id: *client_id,
                        client_id_raw: member.client_id_raw.map(str::to_string),
                        dc_aso: dc_aso.map(str::to_string),
                        ..Default::default()
                    })
                    .collect();

                (
                    member.vote_account.to_string(),
                    ValidatorRecord {
                        vote_account: member.vote_account.to_string(),
                        epoch_stats,
                        net_apy: member.net_apy,
                        avg_take_rate: member.take_rate,
                        unique_delegators: member.unique_delegators,
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

    fn group<'a>(groups: &'a ValidatorGroups, key: &str) -> &'a ValidatorGroupRecord {
        groups
            .groups
            .iter()
            .find(|group| group.key == key)
            .unwrap_or_else(|| panic!("no group for {key:?} in {:?}", keys(groups)))
    }

    // client-ids.csv ids, labelled by `ClientId::groupings`: 3 is `Agave`, 2 `Frankendancer`,
    // 6 `Agave + JitoBAM` — which is the agave lineage, like 3.
    const AGAVE: Option<u16> = Some(3);
    const FRANKENDANCER: Option<u16> = Some(2);
    const JITO_BAM: Option<u16> = Some(6);

    #[test]
    fn label_and_lineage_group_the_same_validators_differently() {
        let validators = validators(vec![
            Member::new("agave", last_two_epochs(300, AGAVE, None)),
            Member::new("bam", last_two_epochs(200, JITO_BAM, None)),
        ]);

        let by_label = aggregate_groups(&validators, GroupKind::ClientLabel);
        assert_eq!(by_label.groups.len(), 2, "{:?}", keys(&by_label));

        // Both are the agave lineage, so the lineage view collapses them into one row — served
        // title-cased, since it sits above variant labels that are.
        let by_lineage = aggregate_groups(&validators, GroupKind::ClientLineage);
        assert_eq!(keys(&by_lineage), vec!["Agave".to_string()]);
        assert_eq!(group(&by_lineage, "Agave").validator_count, 2);
        assert_eq!(group(&by_lineage, "Agave").total_stake, Decimal::from(500));
    }

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
        // 65535 is not in the registry, so the label helpers answer `Unknown`.
        let unregistered = Some(65535);
        let validators = validators(vec![
            Member::new("blank", last_two_epochs(100, None, Some("   "))),
            Member::new(
                "unknown",
                last_two_epochs(100, unregistered, Some("Unknown")),
            ),
            // `Unknown(999)` is what an RPC renders for an id absent from its own table, and 999 is
            // absent from ours too, so nothing about it names a client.
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
    fn a_client_the_registry_does_not_know_keeps_the_name_the_node_reported() {
        // A name absent from client-ids.csv: the RPC could render it, our registry cannot place it.
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
    fn stake_shares_sum_to_one_including_the_unclassified_bucket() {
        let validators = validators(vec![
            Member::new("agave", last_two_epochs(700, AGAVE, None)),
            Member::new("unclassified", last_two_epochs(300, None, None)),
        ]);

        let groups = aggregate_groups(&validators, GroupKind::ClientLabel);
        assert_eq!(groups.total_activated_stake, Decimal::from(1000));
        assert!((group(&groups, UNKNOWN_GROUP).stake_share - 0.3).abs() < 1e-12);
        let total: f64 = groups.groups.iter().map(|group| group.stake_share).sum();
        assert!((total - 1.0).abs() < 1e-12, "{total}");
    }

    #[test]
    fn net_apy_is_stake_weighted_and_drops_dust_from_both_sides() {
        let validators = validators(vec![
            Member {
                net_apy: Some(0.07),
                ..Member::new("paying", last_two_epochs(100, AGAVE, None))
            },
            Member {
                // A full-commission validator reports dust rather than an exact zero.
                net_apy: Some(4.98e-10),
                ..Member::new("dust", last_two_epochs(900, AGAVE, None))
            },
        ]);

        let groups = aggregate_groups(&validators, GroupKind::ClientLabel);
        let net_apy = group(&groups, "Agave").net_apy.unwrap();
        assert!(
            (net_apy - 0.07).abs() < 1e-12,
            "dust stake must leave the denominator too, got {net_apy}"
        );
    }

    #[test]
    fn net_apy_is_none_when_no_member_reports_a_rate() {
        let validators = validators(vec![Member::new(
            "agave",
            last_two_epochs(100, AGAVE, None),
        )]);
        assert!(group(
            &aggregate_groups(&validators, GroupKind::ClientLabel),
            "Agave"
        )
        .net_apy
        .is_none());
    }

    #[test]
    fn take_rate_drops_full_takers_and_keeps_the_ones_just_under() {
        let validators = validators(vec![
            Member {
                take_rate: Some(0.9999),
                ..Member::new("nearly", last_two_epochs(100, AGAVE, None))
            },
            Member {
                take_rate: Some(0.999999978),
                ..Member::new("full", last_two_epochs(900, AGAVE, None))
            },
        ]);

        let take_rate = group(
            &aggregate_groups(&validators, GroupKind::ClientLabel),
            "Agave",
        )
        .take_rate
        .unwrap();
        assert!((take_rate - 0.9999).abs() < 1e-12, "{take_rate}");
    }

    #[test]
    fn delegators_sum_and_stay_none_when_nobody_reports() {
        let counted = validators(vec![
            Member {
                unique_delegators: Some(12),
                ..Member::new("one", last_two_epochs(100, AGAVE, None))
            },
            Member::new("two", last_two_epochs(100, AGAVE, None)),
        ]);
        assert_eq!(
            group(&aggregate_groups(&counted, GroupKind::ClientLabel), "Agave").delegator_count,
            Some(12)
        );

        let uncounted = validators(vec![Member::new("one", last_two_epochs(100, AGAVE, None))]);
        assert_eq!(
            group(
                &aggregate_groups(&uncounted, GroupKind::ClientLabel),
                "Agave"
            )
            .delegator_count,
            None,
            "no member reporting must not read as zero delegators"
        );
    }

    /// Epoch 100 is current, 96 is ~8 days old and 85 ~30 days old, so both windows resolve.
    fn epochs_spanning_both_windows(
        stake_now: i64,
        stake_short: i64,
        stake_long: i64,
        client: Option<u16>,
        client_then: Option<u16>,
    ) -> Vec<EpochSpec> {
        vec![
            (CURRENT_EPOCH, stake_now, client, None),
            (PREVIOUS_EPOCH, stake_now, client, None),
            (96, stake_short, client_then, None),
            (85, stake_long, client_then, None),
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
    fn rows_describe_the_last_epoch_not_an_average_over_history() {
        let validators = validators(vec![Member::new(
            "shrank",
            vec![
                (CURRENT_EPOCH, 100, AGAVE, None),
                (PREVIOUS_EPOCH, 900, AGAVE, None),
            ],
        )]);

        assert_eq!(
            group(
                &aggregate_groups(&validators, GroupKind::ClientLabel),
                "Agave"
            )
            .total_stake,
            Decimal::from(100)
        );
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
    fn provider_names_differing_only_in_case_are_one_group() {
        // The geolocation source re-cases names between epochs; two rows for one company would also
        // split its stake and read as a migration.
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
    fn a_re_cased_name_does_not_read_as_a_migration() {
        let validators = validators(vec![Member::new(
            "recased",
            vec![
                (CURRENT_EPOCH, 500, AGAVE, Some("Retn Limited")),
                (PREVIOUS_EPOCH, 500, AGAVE, Some("Retn Limited")),
                (96, 500, AGAVE, Some("RETN Limited")),
                (85, 500, AGAVE, Some("RETN Limited")),
            ],
        )]);

        let groups = aggregate_groups(&validators, GroupKind::ProviderAso);
        assert_eq!(
            group(&groups, "Retn Limited").stake_delta_7d,
            Some(Decimal::ZERO),
            "the company neither gained nor lost stake, only its spelling changed"
        );
    }

    #[test]
    fn no_delta_against_an_epoch_that_classified_nothing() {
        // Client ids only reach back to the epoch collection started; before that every validator was
        // unclassified, and every client would read as having appeared from nothing.
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

        // The provider view of the same epochs is classified throughout, so it still gets its deltas.
        let providers = aggregate_groups(&validators, GroupKind::ProviderAso);
        assert_eq!(
            group(&providers, "Hetzner").stake_delta_7d,
            Some(Decimal::ZERO)
        );
    }

    #[test]
    fn a_validator_the_list_does_not_serve_counts_nowhere() {
        // Present in the last epoch but neither voting nor staked: `/validators` drops it, so a group
        // must not count it either, or the shares describe a population no consumer can total.
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
        // The list keeps a voting validator with no stake, so the groups have to as well.
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
    fn an_empty_set_aggregates_to_nothing() {
        let groups = aggregate_groups(&Default::default(), GroupKind::ClientLabel);
        assert!(groups.groups.is_empty());
        assert_eq!(groups.current_epoch, None);
        assert_eq!(groups.total_activated_stake, Decimal::ZERO);
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
        // Both entry points share one epoch resolution; a drift between them would serve one number
        // on an endpoint and another to anything calling the aggregation directly.
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

        let lineages = aggregate_groups(&validators, GroupKind::ClientLineage);
        assert_eq!(
            lineages
                .groups
                .iter()
                .map(|group| group.key.clone())
                .collect::<Vec<_>>(),
            all.clients
                .nodes
                .iter()
                .map(|node| node.group.key.clone())
                .collect::<Vec<_>>(),
            "the tree's parents are the lineage grouping"
        );
        assert_eq!(
            lineages.groups.first().map(|group| group.stake_delta_7d),
            all.clients
                .nodes
                .first()
                .map(|node| node.group.stake_delta_7d)
        );
        assert_eq!(lineages.current_epoch, all.clients.current_epoch);
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

        let eligible = eligible(&validators);
        let epochs = epochs(&eligible, Utc::now()).unwrap();
        assert_eq!(epochs.current, 100);
        // Epochs run ~2 days: 96 ended ~8 days ago, 95 ~10, so 96 is the newest one old enough.
        assert_eq!(epochs.delta_7d, Some(96));
        assert_eq!(epochs.delta_30d, Some(85));
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
    fn variants_sit_under_the_client_they_are_built_from() {
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
            "a variant with no vendor modification keys as the client itself"
        );
        assert_eq!(
            child_keys(&tree.nodes[1]),
            vec!["Frankendancer".to_string()]
        );
    }

    #[test]
    fn a_client_is_aggregated_over_its_own_validators_not_summed_from_its_variants() {
        // Two variants of one client with different rates: the parent has to be the stake-weighted
        // rate over both, which is not the mean of the two child rates.
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
    fn a_variant_the_registry_cannot_place_lands_under_the_unclassified_parent() {
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
            "the variant keeps the name the node reported even though its client is unknown"
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
    fn a_variant_switch_inside_one_client_leaves_the_parent_delta_flat() {
        // Agave + Jito -> Agave + JitoBAM is a variant change, not a client change: the children move,
        // the client does not.
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
            "the variant it moved to gained all of it"
        );
    }

    #[test]
    fn aggregate_all_serves_both_the_client_tree_and_the_providers() {
        let all = aggregate_all(&validators(vec![Member::new(
            "one",
            last_two_epochs(100, AGAVE, Some("Hetzner")),
        )]));

        assert_eq!(
            all.clients
                .nodes
                .iter()
                .map(|node| node.group.key.clone())
                .collect::<Vec<_>>(),
            vec!["Agave".to_string()]
        );
        assert_eq!(keys(&all.providers), vec!["Hetzner".to_string()]);
    }

    #[test]
    fn keys_compare_case_insensitively_and_totally() {
        assert_eq!(
            compare_keys("hetzner", "Latitude"),
            Ordering::Less,
            "comparison ignores case, so casing cannot reorder a page"
        );
        assert_eq!(
            compare_keys("Hetzner", "hetzner"),
            Ordering::Less,
            "two spellings still order deterministically, or a page could repeat a row"
        );
        assert_eq!(compare_keys("Agave", "Agave"), Ordering::Equal);
    }
}
