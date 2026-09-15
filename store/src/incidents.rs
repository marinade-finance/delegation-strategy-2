use crate::dto;
use chrono::{DateTime, Utc};
use collect::validator_version::ValidatorVersion;
use rust_decimal::prelude::*;
use std::collections::HashMap;

/// Leader slots an epoch needs before its block production is judged at all. Below this there is
/// not enough signal; covers 99.2% of stake.
pub const MIN_LEADER_SLOTS: u64 = 64;

/// Missed slots an epoch needs before it counts as an incident. Solana assigns leader slots in
/// batches of 4, so a sub-turn miss is usually the cluster following a different fork.
pub const MIN_MISSED_SLOTS: u64 = 4;

/// The bar in a healthy epoch: produce 99% of assigned leader slots.
pub const MIN_SKIP_RATE_THRESHOLD: f64 = 0.01;

/// How far above the cluster's own skip rate the bar sits while the network is degraded.
pub const CLUSTER_SKIP_RATE_MULTIPLIER: f64 = 10.0;

/// Ceiling on the bar, so a degraded network can never push it somewhere non-physical. Uncapped,
/// the 500-699 era would have moved it past 100%.
pub const MAX_SKIP_RATE_THRESHOLD: f64 = 0.05;

/// Under this many seconds a `DOWN` interval is restart noise, not an incident.
pub const DEFAULT_MIN_INCIDENT_DOWNTIME_SECONDS: u64 = 180;

// Validator raised commission to this or more in an epoch -> incident
pub const COMMISSION_SPIKE_THRESHOLD_PERCENTAGE: u8 = 90;

pub const OUTDATED_CLIENT_NEWER_STAKE_SHARE: f64 = 0.80;

pub const DEFAULT_INCIDENT_TYPES: &[IncidentType] = &[IncidentType::Downtime];

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IncidentType {
    Downtime,
    BlockProduction,
    CommissionSpike,
    OutdatedClient,
}

impl IncidentType {
    /// Takes the names the response emits under `incident_type`.
    pub fn parse_list(types: &str) -> Result<Vec<Self>, String> {
        types
            .split(',')
            .map(|name| match name.trim() {
                "Downtime" => Ok(Self::Downtime),
                "BlockProduction" => Ok(Self::BlockProduction),
                "CommissionSpike" => Ok(Self::CommissionSpike),
                "OutdatedClient" => Ok(Self::OutdatedClient),
                other => Err(other.to_string()),
            })
            .collect()
    }
}

/// One `DOWN` interval as `uptimes` recorded it.
#[derive(Debug, Clone)]
pub struct DowntimeInterval {
    pub epoch: u64,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    pub downtime_seconds: u64,
}

/// One closed epoch's leader slot counters, and the cluster figure they are judged against.
#[derive(Debug, Clone)]
pub struct EpochBlockProduction {
    pub epoch: u64,
    pub epoch_start_at: DateTime<Utc>,
    pub epoch_end_at: DateTime<Utc>,
    pub leader_slots: u64,
    /// Clamped to `leader_slots`: a node reporting more blocks than slots is upstream noise.
    pub blocks_produced: u64,
    pub cluster_skip_rate: f64,
}

impl EpochBlockProduction {
    /// `None` with no leader slots to divide by, and with no `epochs` row yet: that is the epoch
    /// still running, whose counters cover only the slots elapsed so far.
    pub fn new(stats: &dto::ValidatorEpochStats, cluster_skip_rate: f64) -> Option<Self> {
        let (Some(epoch_start_at), Some(epoch_end_at)) = (stats.epoch_start_at, stats.epoch_end_at)
        else {
            return None;
        };
        if stats.leader_slots == 0 {
            return None;
        }

        Some(Self {
            epoch: stats.epoch,
            epoch_start_at,
            epoch_end_at,
            leader_slots: stats.leader_slots,
            blocks_produced: stats.blocks_produced.min(stats.leader_slots),
            cluster_skip_rate,
        })
    }

    pub fn missed_slots(&self) -> u64 {
        self.leader_slots - self.blocks_produced
    }

    pub fn skip_rate(&self) -> f64 {
        self.missed_slots() as f64 / self.leader_slots as f64
    }

    /// The bar the epoch had to clear, as a fraction.
    pub fn threshold(&self) -> f64 {
        (CLUSTER_SKIP_RATE_MULTIPLIER * self.cluster_skip_rate)
            .clamp(MIN_SKIP_RATE_THRESHOLD, MAX_SKIP_RATE_THRESHOLD)
    }

    /// Whether the validator broke the block production rule that epoch. The caller's floors can
    /// only tighten the rule: they never reach under `MIN_LEADER_SLOTS` or `MIN_MISSED_SLOTS`.
    pub fn counts_as_incident(&self, filters: &IncidentFilters) -> bool {
        self.leader_slots >= filters.min_leader_slots.unwrap_or(0).max(MIN_LEADER_SLOTS)
            && self.missed_slots() >= filters.min_missed_slots.unwrap_or(0).max(MIN_MISSED_SLOTS)
            && self.skip_rate() >= self.threshold()
    }

    /// The numbers as the response carries them, judged under the caller's floors.
    pub fn detail(&self, filters: &IncidentFilters) -> dto::BlockProductionDetail {
        dto::BlockProductionDetail {
            leader_slots: self.leader_slots,
            blocks_produced: self.blocks_produced,
            missed_slots: self.missed_slots(),
            skip_rate: self.skip_rate(),
            cluster_skip_rate: self.cluster_skip_rate,
            threshold: self.threshold(),
            counts_as_incident: self.counts_as_incident(filters),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommissionRaise {
    pub epoch: u64,
    pub epoch_slot: u64,
    pub changed_at: DateTime<Utc>,
    pub commission_before: u8,
    pub commission_after: u8,
}

#[derive(Debug, Clone)]
pub struct EpochClientVersion {
    pub epoch: u64,
    pub epoch_start_at: DateTime<Utc>,
    pub epoch_end_at: DateTime<Utc>,
    pub version: String,
    pub client_lineage: String,
    /// Share of the lineage's stake on a strictly newer version, as a fraction.
    pub newer_stake_share: f64,
}

impl EpochClientVersion {
    pub fn counts_as_incident(&self) -> bool {
        self.newer_stake_share >= OUTDATED_CLIENT_NEWER_STAKE_SHARE
    }
}

/// `None` for an epoch still running, a client id absent from the registry, or a version string
/// nothing can parse.
fn epoch_client_version(stats: &dto::ValidatorEpochStats) -> Option<(ValidatorVersion, String)> {
    let version = stats.version.as_deref()?.parse().ok()?;
    let lineage = stats.client_lineage.clone()?;
    stats.epoch_start_at?;
    stats.epoch_end_at?;
    Some((version, lineage))
}

/// Keyed by epoch and lineage, then by the version the share is measured from. A validator
/// `epoch_client_version` rejects weighs on neither side.
pub fn newer_stake_shares<'a>(
    records: impl IntoIterator<Item = &'a dto::ValidatorRecord>,
) -> HashMap<(u64, String), HashMap<String, f64>> {
    let mut stake: HashMap<(u64, String), HashMap<ValidatorVersion, Decimal>> = Default::default();

    for stats in records.into_iter().flat_map(|record| &record.epoch_stats) {
        let Some((version, lineage)) = epoch_client_version(stats) else {
            continue;
        };
        *stake
            .entry((stats.epoch, lineage))
            .or_default()
            .entry(version)
            .or_default() += stats.activated_stake;
    }

    stake
        .into_iter()
        .map(|(key, by_version)| {
            let total: Decimal = by_version.values().sum();
            let mut versions: Vec<(ValidatorVersion, Decimal)> = by_version.into_iter().collect();
            // Newest first, so the running sum ahead of each version is the stake above it.
            versions.sort_by(|(a, _), (b, _)| b.cmp(a));

            let mut newer = Decimal::ZERO;
            let mut shares = HashMap::new();
            for (version, version_stake) in versions {
                let share = if total.is_zero() {
                    0.0
                } else {
                    (newer / total).to_f64().unwrap_or(0.0)
                };
                shares.insert(version.as_str().to_string(), share);
                newer += version_stake;
            }
            (key, shares)
        })
        .collect()
}

/// Oldest first, across every epoch the records carry so the grace still sees a predecessor
/// outside the window.
pub fn outdated_epochs(
    record: &dto::ValidatorRecord,
    newer_stake_shares: &HashMap<(u64, String), HashMap<String, f64>>,
) -> Vec<EpochClientVersion> {
    let mut outdated: Vec<EpochClientVersion> = record
        .epoch_stats
        .iter()
        .filter_map(|stats| {
            let (version, client_lineage) = epoch_client_version(stats)?;
            let newer_stake_share = *newer_stake_shares
                .get(&(stats.epoch, client_lineage.clone()))?
                .get(version.as_str())?;

            Some(EpochClientVersion {
                epoch: stats.epoch,
                epoch_start_at: stats.epoch_start_at?,
                epoch_end_at: stats.epoch_end_at?,
                version: version.as_str().to_string(),
                client_lineage,
                newer_stake_share,
            })
        })
        .filter(EpochClientVersion::counts_as_incident)
        .collect();

    outdated.sort_by_key(|epoch| epoch.epoch);
    outdated
}

/// An epoch counts only where the one before it was over the bar too. An epoch with nothing usable
/// reported is no breach, so it closes the run.
pub fn outdated_after_grace(
    outdated: Vec<EpochClientVersion>,
    epochs: std::ops::RangeInclusive<u64>,
) -> Vec<EpochClientVersion> {
    let breached: std::collections::HashSet<u64> =
        outdated.iter().map(|epoch| epoch.epoch).collect();
    outdated
        .into_iter()
        .filter(|epoch| epochs.contains(&epoch.epoch))
        .filter(|epoch| {
            epoch
                .epoch
                .checked_sub(1)
                .is_some_and(|before| breached.contains(&before))
        })
        .collect()
}

/// Window and floors one response is judged under.
#[derive(Debug, Clone)]
pub struct IncidentFilters {
    /// Oldest epoch to report, inclusive.
    pub from_epoch: u64,
    pub min_downtime_seconds: u64,
    pub min_missed_slots: Option<u64>,
    pub min_leader_slots: Option<u64>,
    /// `None` serves every kind.
    pub types: Option<Vec<IncidentType>>,
}

impl Default for IncidentFilters {
    fn default() -> Self {
        Self {
            from_epoch: 0,
            min_downtime_seconds: DEFAULT_MIN_INCIDENT_DOWNTIME_SECONDS,
            min_missed_slots: None,
            min_leader_slots: None,
            types: None,
        }
    }
}

impl IncidentFilters {
    fn wants(&self, incident_type: IncidentType) -> bool {
        self.types
            .as_ref()
            .is_none_or(|types| types.contains(&incident_type))
    }
}

/// One validator's raw material: nothing judged, nothing merged.
#[derive(Debug, Clone, Default)]
pub struct ValidatorIncidentRecords {
    pub downtimes: Vec<DowntimeInterval>,
    pub block_production: Vec<EpochBlockProduction>,
    pub commission_raises: Vec<CommissionRaise>,
    pub outdated_clients: Vec<EpochClientVersion>,
}

impl ValidatorIncidentRecords {
    /// An epoch's block production is reported once: on that epoch's downtime records if any are
    /// served, otherwise as a record of its own. A commission spike is never folded into either.
    pub fn into_response_incidents(&self, filters: &IncidentFilters) -> Vec<dto::IncidentRecord> {
        let mut incidents: Vec<dto::IncidentRecord> = Vec::new();
        // Epochs whose block production a downtime record already carries.
        let mut carried: Vec<u64> = Vec::new();

        if filters.wants(IncidentType::Downtime) {
            for downtime in self.downtimes.iter().filter(|downtime| {
                downtime.epoch >= filters.from_epoch
                    && downtime.downtime_seconds >= filters.min_downtime_seconds
            }) {
                carried.push(downtime.epoch);
                incidents.push(dto::IncidentRecord {
                    epoch: downtime.epoch,
                    detail: dto::IncidentDetail::Downtime {
                        start_at: downtime.start_at,
                        end_at: downtime.end_at,
                        downtime_seconds: downtime.downtime_seconds,
                        block_production: self
                            .epoch_block_production(downtime.epoch)
                            .map(|production| production.detail(filters)),
                    },
                });
            }
        }

        if filters.wants(IncidentType::BlockProduction) {
            for production in self
                .block_production
                .iter()
                .filter(|production| production.epoch >= filters.from_epoch)
                .filter(|production| !carried.contains(&production.epoch))
            {
                if production.counts_as_incident(filters) {
                    incidents.push(dto::IncidentRecord {
                        epoch: production.epoch,
                        detail: dto::IncidentDetail::BlockProduction {
                            epoch_start_at: production.epoch_start_at,
                            epoch_end_at: production.epoch_end_at,
                            block_production: production.detail(filters),
                        },
                    });
                }
            }
        }

        if filters.wants(IncidentType::CommissionSpike) {
            for raise in self
                .commission_raises
                .iter()
                .filter(|raise| raise.epoch >= filters.from_epoch)
            {
                incidents.push(dto::IncidentRecord {
                    epoch: raise.epoch,
                    detail: dto::IncidentDetail::CommissionSpike {
                        commission_before: raise.commission_before,
                        commission_after: raise.commission_after,
                        changed_at: raise.changed_at,
                        epoch_slot: raise.epoch_slot,
                    },
                });
            }
        }

        if filters.wants(IncidentType::OutdatedClient) {
            for outdated in self
                .outdated_clients
                .iter()
                .filter(|outdated| outdated.epoch >= filters.from_epoch)
            {
                incidents.push(dto::IncidentRecord {
                    epoch: outdated.epoch,
                    detail: dto::IncidentDetail::OutdatedClient {
                        epoch_start_at: outdated.epoch_start_at,
                        epoch_end_at: outdated.epoch_end_at,
                        version: outdated.version.clone(),
                        client_lineage: outdated.client_lineage.clone(),
                        newer_stake_share: outdated.newer_stake_share,
                    },
                });
            }
        }

        incidents.sort_by_key(|incident| (incident.epoch, incident.detail.started_at()));
        incidents
    }

    fn epoch_block_production(&self, epoch: u64) -> Option<&EpochBlockProduction> {
        self.block_production
            .iter()
            .find(|production| production.epoch == epoch)
    }
}

/// Keyed by vote account.
#[derive(Debug, Clone, Default)]
pub struct ValidatorIncidents(HashMap<String, ValidatorIncidentRecords>);

impl ValidatorIncidents {
    pub fn records(&mut self, vote_account: &str) -> &mut ValidatorIncidentRecords {
        self.0.entry(vote_account.to_string()).or_default()
    }

    pub fn get(&self, vote_account: &str) -> Option<&ValidatorIncidentRecords> {
        self.0.get(vote_account)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Empty for a validator with no incident material.
    pub fn into_response_incidents(
        &self,
        vote_account: &str,
        filters: &IncidentFilters,
    ) -> Vec<dto::IncidentRecord> {
        self.get(vote_account)
            .map(|records| records.into_response_incidents(filters))
            .unwrap_or_default()
    }
}

/// Total missed slots over total leader slots per epoch, across the validators that were evaluable
/// that epoch. Epochs where nobody reached `MIN_LEADER_SLOTS` are left out.
pub fn cluster_skip_rates<'a>(
    records: impl IntoIterator<Item = &'a dto::ValidatorRecord>,
) -> HashMap<u64, f64> {
    let mut totals: HashMap<u64, (u64, u64)> = Default::default();

    for stats in records.into_iter().flat_map(|record| &record.epoch_stats) {
        if stats.leader_slots < MIN_LEADER_SLOTS {
            continue;
        }
        let (leader_slots, blocks_produced) = totals.entry(stats.epoch).or_default();
        *leader_slots += stats.leader_slots;
        // A node reporting more blocks than slots is upstream noise; it must not mint slots here.
        *blocks_produced += stats.blocks_produced.min(stats.leader_slots);
    }

    // Every epoch in here cleared MIN_LEADER_SLOTS above, so there is no zero to divide by.
    totals
        .into_iter()
        .map(|(epoch, (leader_slots, blocks_produced))| {
            (
                epoch,
                (leader_slots - blocks_produced) as f64 / leader_slots as f64,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPOCH: u64 = 100;

    fn stats(epoch: u64, leader_slots: u64, blocks_produced: u64) -> dto::ValidatorEpochStats {
        let epoch_start_at: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        dto::ValidatorEpochStats {
            epoch,
            leader_slots,
            blocks_produced,
            epoch_start_at: Some(epoch_start_at),
            epoch_end_at: Some(epoch_start_at + chrono::Duration::days(2)),
            ..Default::default()
        }
    }

    fn validator(epoch_stats: Vec<dto::ValidatorEpochStats>) -> dto::ValidatorRecord {
        dto::ValidatorRecord {
            epoch_stats,
            ..Default::default()
        }
    }

    fn production(
        leader_slots: u64,
        blocks_produced: u64,
        cluster_skip_rate: f64,
    ) -> Option<EpochBlockProduction> {
        EpochBlockProduction::new(
            &stats(EPOCH, leader_slots, blocks_produced),
            cluster_skip_rate,
        )
    }

    fn detail(
        leader_slots: u64,
        blocks_produced: u64,
        cluster_skip_rate: f64,
    ) -> Option<dto::BlockProductionDetail> {
        production(leader_slots, blocks_produced, cluster_skip_rate)
            .map(|production| production.detail(&Default::default()))
    }

    /// The numbers, but only where they broke the rule: what the incidents themselves are built on.
    fn counts_as_incident(
        leader_slots: u64,
        blocks_produced: u64,
        cluster_skip_rate: f64,
    ) -> Option<dto::BlockProductionDetail> {
        detail(leader_slots, blocks_produced, cluster_skip_rate)
            .filter(|detail| detail.counts_as_incident)
    }

    #[test]
    fn an_epoch_under_the_leader_slot_gate_is_not_evaluated() {
        assert!(counts_as_incident(63, 0, 0.0).is_none());
    }

    #[test]
    fn an_epoch_under_the_leader_slot_gate_still_reports_its_numbers() {
        // A downtime record still reports the numbers; only the verdict needs the gate.
        let detail = detail(8, 5, 0.0).unwrap();

        assert_eq!(detail.missed_slots, 3);
        assert!(!detail.counts_as_incident);
    }

    fn floors(min_missed_slots: Option<u64>, min_leader_slots: Option<u64>) -> IncidentFilters {
        IncidentFilters {
            min_missed_slots,
            min_leader_slots,
            ..Default::default()
        }
    }

    #[test]
    fn a_caller_missed_slot_floor_tightens_the_rule_but_cannot_loosen_it() {
        let production = production(64, 60, 0.0).unwrap();

        assert!(production.counts_as_incident(&Default::default()));
        assert!(!production.counts_as_incident(&floors(Some(5), None)));
        // 4 missed is the rule's own floor, and no caller gets under it.
        assert!(production.counts_as_incident(&floors(Some(0), None)));
    }

    #[test]
    fn a_caller_leader_slot_floor_tightens_the_rule_but_cannot_loosen_it() {
        let production = production(64, 60, 0.0).unwrap();

        assert!(production.counts_as_incident(&Default::default()));
        assert!(!production.counts_as_incident(&floors(None, Some(65))));
        // 64 slots is the rule's own floor, and no caller gets under it.
        assert!(production.counts_as_incident(&floors(None, Some(8))));
    }

    #[test]
    fn an_epoch_with_no_leader_slots_has_no_block_production() {
        assert!(detail(0, 0, 0.0).is_none());
    }

    #[test]
    fn the_epoch_in_flight_has_no_block_production() {
        let mut running = stats(EPOCH, 1000, 940);
        running.epoch_end_at = None;

        assert!(EpochBlockProduction::new(&running, 0.0).is_none());
    }

    #[test]
    fn more_blocks_than_slots_is_clamped_rather_than_wrapping() {
        let detail = detail(64, 70, 0.0).unwrap();

        assert_eq!(detail.blocks_produced, 64);
        assert_eq!(detail.missed_slots, 0);
    }

    #[test]
    fn a_sub_leader_turn_miss_is_not_an_incident() {
        // 3 of 64 is 4.7%, well over the bar, but under one full leader turn.
        assert!(counts_as_incident(64, 61, 0.0).is_none());
    }

    #[test]
    fn a_full_leader_turn_missed_over_the_bar_is_an_incident() {
        let detail = counts_as_incident(64, 60, 0.0).unwrap();

        assert_eq!(detail.missed_slots, 4);
        assert_eq!(detail.threshold, 0.01);
    }

    #[test]
    fn an_epoch_under_the_bar_is_no_incident_even_with_a_full_turn_missed() {
        // 4 of 800 is 0.5%, under the 1% bar.
        assert!(counts_as_incident(800, 796, 0.0).is_none());
    }

    #[test]
    fn a_degraded_cluster_lifts_the_bar_out_from_under_a_validator() {
        // 2% skipped: an incident in a healthy epoch, not in one the cluster skipped 0.413% of.
        assert!(counts_as_incident(1000, 980, 0.0).is_some());
        assert!(counts_as_incident(1000, 980, 0.004_13).is_none());
    }

    #[test]
    fn the_cap_keeps_the_bar_physical_when_the_cluster_is_badly_degraded() {
        // Uncapped, 10 x 3.5% would be a 35% bar and this epoch would read as all clear.
        let detail = counts_as_incident(1000, 940, 0.035).unwrap();
        assert_eq!(detail.threshold, MAX_SKIP_RATE_THRESHOLD);
    }

    #[test]
    fn cluster_skip_rate_reads_only_the_evaluable_validators() {
        let validators = [
            validator(vec![stats(100, 1000, 990)]),
            validator(vec![stats(100, 1000, 990)]),
            // Under the gate: its 100% skip rate must not move the cluster figure.
            validator(vec![stats(100, 32, 0)]),
        ];
        assert_eq!(cluster_skip_rates(&validators).get(&100), Some(&0.01));
    }

    fn downtime(epoch: u64, downtime_seconds: u64) -> DowntimeInterval {
        let start_at: DateTime<Utc> = "2026-01-01T12:00:00Z".parse().unwrap();
        DowntimeInterval {
            epoch,
            start_at: start_at + chrono::Duration::seconds(downtime_seconds as i64),
            end_at: start_at + chrono::Duration::seconds(2 * downtime_seconds as i64),
            downtime_seconds,
        }
    }

    /// An epoch that missed 60 of its 1000 leader slots: 6%, over any bar the rule can set.
    fn breached(epoch: u64) -> EpochBlockProduction {
        EpochBlockProduction::new(&stats(epoch, 1000, 940), 0.0).unwrap()
    }

    /// An epoch that produced every slot it was given.
    fn clean(epoch: u64) -> EpochBlockProduction {
        EpochBlockProduction::new(&stats(epoch, 1000, 1000), 0.0).unwrap()
    }

    fn records(
        downtimes: Vec<DowntimeInterval>,
        block_production: Vec<EpochBlockProduction>,
    ) -> ValidatorIncidentRecords {
        ValidatorIncidentRecords {
            downtimes,
            block_production,
            ..Default::default()
        }
    }

    /// A raise from 5% to 100%, at 06:00 of the epoch the other fixtures place at 00:00.
    fn raise(epoch: u64) -> CommissionRaise {
        CommissionRaise {
            epoch,
            epoch_slot: 1000,
            changed_at: "2026-01-01T06:00:00Z".parse().unwrap(),
            commission_before: 5,
            commission_after: 100,
        }
    }

    fn outdated(epoch: u64, newer_stake_share: f64) -> EpochClientVersion {
        let epoch_start_at: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        EpochClientVersion {
            epoch,
            epoch_start_at,
            epoch_end_at: epoch_start_at + chrono::Duration::days(2),
            version: "4.2.0".to_string(),
            client_lineage: "agave".to_string(),
            newer_stake_share,
        }
    }

    /// One epoch of one validator, as the version comparison reads it.
    fn client(
        epoch: u64,
        version: Option<&str>,
        lineage: Option<&str>,
        stake: u64,
    ) -> dto::ValidatorEpochStats {
        let epoch_start_at: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        dto::ValidatorEpochStats {
            epoch,
            epoch_start_at: Some(epoch_start_at),
            epoch_end_at: Some(epoch_start_at + chrono::Duration::days(2)),
            version: version.map(str::to_string),
            client_lineage: lineage.map(str::to_string),
            activated_stake: Decimal::from(stake),
            ..Default::default()
        }
    }

    fn agave(epoch: u64, version: &str, stake: u64) -> dto::ValidatorEpochStats {
        client(epoch, Some(version), Some("agave"), stake)
    }

    /// Epochs the validator is served as outdated for, given the whole cluster's epoch stats.
    fn outdated_served(
        subject: Vec<dto::ValidatorEpochStats>,
        others: Vec<Vec<dto::ValidatorEpochStats>>,
        epochs: std::ops::RangeInclusive<u64>,
    ) -> Vec<u64> {
        let subject = validator(subject);
        let cluster: Vec<dto::ValidatorRecord> = std::iter::once(subject.clone())
            .chain(others.into_iter().map(validator))
            .collect();
        let shares = newer_stake_shares(&cluster);

        outdated_after_grace(outdated_epochs(&subject, &shares), epochs)
            .iter()
            .map(|epoch| epoch.epoch)
            .collect()
    }

    fn served_types(incidents: &[dto::IncidentRecord]) -> Vec<&'static str> {
        incidents
            .iter()
            .map(|incident| match incident.detail {
                dto::IncidentDetail::Downtime { .. } => "Downtime",
                dto::IncidentDetail::BlockProduction { .. } => "BlockProduction",
                dto::IncidentDetail::CommissionSpike { .. } => "CommissionSpike",
                dto::IncidentDetail::OutdatedClient { .. } => "OutdatedClient",
            })
            .collect()
    }

    fn carried_numbers(incident: &dto::IncidentRecord) -> Option<&dto::BlockProductionDetail> {
        match &incident.detail {
            dto::IncidentDetail::Downtime {
                block_production, ..
            } => block_production.as_ref(),
            dto::IncidentDetail::BlockProduction {
                block_production, ..
            } => Some(block_production),
            dto::IncidentDetail::CommissionSpike { .. } => None,
            dto::IncidentDetail::OutdatedClient { .. } => None,
        }
    }

    #[test]
    fn a_restart_under_the_floor_is_no_incident_of_its_own() {
        let records = records(vec![downtime(EPOCH, 12)], vec![clean(EPOCH)]);

        assert!(records
            .into_response_incidents(&Default::default())
            .is_empty());
    }

    #[test]
    fn a_restart_under_the_floor_in_a_breached_epoch_serves_the_breach() {
        let records = records(vec![downtime(EPOCH, 12)], vec![breached(EPOCH)]);
        let incidents = records.into_response_incidents(&Default::default());

        assert_eq!(served_types(&incidents), vec!["BlockProduction"]);
        assert_eq!(incidents[0].epoch, EPOCH);
    }

    #[test]
    fn three_restarts_under_the_floor_in_a_breached_epoch_serve_one_row() {
        let records = records(
            vec![
                downtime(EPOCH, 12),
                downtime(EPOCH, 13),
                downtime(EPOCH, 14),
            ],
            vec![breached(EPOCH)],
        );

        assert_eq!(
            served_types(&records.into_response_incidents(&Default::default())),
            vec!["BlockProduction"]
        );
    }

    #[test]
    fn an_outage_in_a_breached_epoch_carries_the_numbers() {
        let records = records(vec![downtime(EPOCH, 600)], vec![breached(EPOCH)]);
        let incidents = records.into_response_incidents(&Default::default());

        assert_eq!(served_types(&incidents), vec!["Downtime"]);
        let numbers = carried_numbers(&incidents[0]).expect("the downtime record has them");
        assert_eq!(numbers.missed_slots, 60);
        assert!(numbers.counts_as_incident);
    }

    #[test]
    fn every_outage_of_a_breached_epoch_is_its_own_row() {
        let records = records(
            vec![
                downtime(EPOCH, 600),
                downtime(EPOCH, 700),
                downtime(EPOCH, 800),
            ],
            vec![breached(EPOCH)],
        );
        let incidents = records.into_response_incidents(&Default::default());

        assert_eq!(served_types(&incidents), vec!["Downtime"; 3]);
        assert!(incidents
            .iter()
            .all(|incident| carried_numbers(incident).is_some()));
    }

    #[test]
    fn an_outage_of_a_passing_epoch_carries_numbers_that_are_no_verdict() {
        let records = records(vec![downtime(EPOCH, 600)], vec![clean(EPOCH)]);
        let incidents = records.into_response_incidents(&Default::default());

        let numbers = carried_numbers(&incidents[0]).expect("the downtime record has them");
        assert_eq!(numbers.missed_slots, 0);
        assert!(!numbers.counts_as_incident);
    }

    #[test]
    fn a_breached_epoch_the_validator_stayed_up_for_opens_its_own_row() {
        let records = records(vec![], vec![breached(EPOCH)]);

        assert_eq!(
            served_types(&records.into_response_incidents(&Default::default())),
            vec!["BlockProduction"]
        );
    }

    // The epoch both went down and breached, so each type has something of its own to serve.
    #[test]
    fn each_type_serves_the_epoch_under_its_own_name() {
        let records = ValidatorIncidentRecords {
            downtimes: vec![downtime(EPOCH, 600)],
            block_production: vec![breached(EPOCH)],
            commission_raises: vec![raise(EPOCH)],
            outdated_clients: vec![outdated(EPOCH, 0.9)],
        };

        for (incident_type, served) in [
            (IncidentType::Downtime, "Downtime"),
            (IncidentType::BlockProduction, "BlockProduction"),
            (IncidentType::CommissionSpike, "CommissionSpike"),
            (IncidentType::OutdatedClient, "OutdatedClient"),
        ] {
            let filters = IncidentFilters {
                types: Some(vec![incident_type]),
                ..Default::default()
            };
            assert_eq!(
                served_types(&records.into_response_incidents(&filters)),
                vec![served],
                "{incident_type:?}"
            );
        }
    }

    #[test]
    fn from_epoch_drops_what_sits_before_the_window() {
        let records = records(
            vec![downtime(99, 600), downtime(100, 600)],
            vec![breached(98), breached(99), breached(100)],
        );
        let filters = IncidentFilters {
            from_epoch: 100,
            ..Default::default()
        };

        let incidents = records.into_response_incidents(&filters);
        assert_eq!(
            incidents
                .iter()
                .map(|incident| incident.epoch)
                .collect::<Vec<_>>(),
            vec![100]
        );
    }

    #[test]
    fn a_zero_downtime_floor_serves_every_interval() {
        let records = records(
            vec![downtime(EPOCH, 12), downtime(EPOCH, 13)],
            vec![breached(EPOCH)],
        );
        let filters = IncidentFilters {
            min_downtime_seconds: 0,
            ..Default::default()
        };

        assert_eq!(
            served_types(&records.into_response_incidents(&filters)),
            vec!["Downtime"; 2]
        );
    }

    #[test]
    fn a_caller_floor_over_the_breach_leaves_the_epoch_unserved() {
        let records = records(vec![downtime(EPOCH, 12)], vec![breached(EPOCH)]);
        let filters = IncidentFilters {
            min_missed_slots: Some(61),
            ..Default::default()
        };

        assert!(records.into_response_incidents(&filters).is_empty());
    }

    #[test]
    fn incidents_come_back_oldest_epoch_first() {
        let records = records(
            vec![downtime(101, 600), downtime(99, 600)],
            vec![breached(100)],
        );

        let incidents = records.into_response_incidents(&Default::default());
        assert_eq!(
            incidents
                .iter()
                .map(|incident| incident.epoch)
                .collect::<Vec<_>>(),
            vec![99, 100, 101]
        );
    }

    #[test]
    fn a_spike_is_no_part_of_whether_the_validator_was_up() {
        let records = ValidatorIncidentRecords {
            downtimes: vec![],
            block_production: vec![breached(EPOCH)],
            commission_raises: vec![raise(EPOCH)],
            ..Default::default()
        };

        assert_eq!(
            served_types(&records.into_response_incidents(&Default::default())),
            vec!["BlockProduction", "CommissionSpike"]
        );
    }

    #[test]
    fn an_outage_does_not_swallow_the_epoch_s_spike() {
        let records = ValidatorIncidentRecords {
            downtimes: vec![downtime(EPOCH, 600)],
            block_production: vec![breached(EPOCH)],
            commission_raises: vec![raise(EPOCH)],
            ..Default::default()
        };

        assert_eq!(
            served_types(&records.into_response_incidents(&Default::default())),
            vec!["CommissionSpike", "Downtime"]
        );
    }

    // What a caller naming no types gets.
    #[test]
    fn the_default_types_serve_downtime_alone() {
        let records = ValidatorIncidentRecords {
            downtimes: vec![downtime(EPOCH, 600)],
            block_production: vec![breached(EPOCH)],
            commission_raises: vec![raise(EPOCH)],
            outdated_clients: vec![outdated(EPOCH, 0.9)],
        };
        let filters = IncidentFilters {
            types: Some(DEFAULT_INCIDENT_TYPES.to_vec()),
            ..Default::default()
        };

        assert_eq!(
            served_types(&records.into_response_incidents(&filters)),
            vec!["Downtime"]
        );
    }

    #[test]
    fn from_epoch_drops_a_spike_before_the_window() {
        let records = ValidatorIncidentRecords {
            commission_raises: vec![raise(99), raise(100)],
            ..Default::default()
        };
        let filters = IncidentFilters {
            from_epoch: 100,
            ..Default::default()
        };

        let incidents = records.into_response_incidents(&filters);
        assert_eq!(
            incidents
                .iter()
                .map(|incident| incident.epoch)
                .collect::<Vec<_>>(),
            vec![100]
        );
    }

    #[test]
    fn parse_list_takes_every_name_the_response_emits() {
        assert_eq!(
            IncidentType::parse_list("Downtime, BlockProduction,CommissionSpike,OutdatedClient"),
            Ok(vec![
                IncidentType::Downtime,
                IncidentType::BlockProduction,
                IncidentType::CommissionSpike,
                IncidentType::OutdatedClient,
            ])
        );
        assert_eq!(
            IncidentType::parse_list("Downtime,Commission"),
            Err("Commission".to_string())
        );
    }

    #[test]
    fn the_newest_version_has_nothing_above_it_and_the_oldest_has_everything() {
        let cluster = [
            validator(vec![agave(EPOCH, "4.2.0", 100)]),
            validator(vec![agave(EPOCH, "4.1.0", 300)]),
        ];
        let shares = newer_stake_shares(&cluster);
        let agave_epoch = shares.get(&(EPOCH, "agave".to_string())).unwrap();

        assert_eq!(agave_epoch.get("4.2.0"), Some(&0.0));
        assert_eq!(agave_epoch.get("4.1.0"), Some(&0.25));
    }

    #[test]
    fn the_share_is_stake_weighted_not_one_vote_each() {
        // Four validators newer, but they carry a twentieth of the stake between them.
        let cluster = [
            validator(vec![agave(EPOCH, "4.1.0", 1000)]),
            validator(vec![agave(EPOCH, "4.2.0", 10)]),
            validator(vec![agave(EPOCH, "4.2.0", 10)]),
            validator(vec![agave(EPOCH, "4.2.0", 10)]),
            validator(vec![agave(EPOCH, "4.2.0", 10)]),
        ];
        let shares = newer_stake_shares(&cluster);

        let behind = shares.get(&(EPOCH, "agave".to_string())).unwrap()["4.1.0"];
        assert!(behind < 0.05, "{behind}");
    }

    #[test]
    fn a_lineage_is_never_judged_against_another() {
        let cluster = [
            validator(vec![agave(EPOCH, "4.2.0", 100)]),
            validator(vec![client(EPOCH, Some("26.8.2"), Some("firedancer"), 900)]),
        ];
        let shares = newer_stake_shares(&cluster);

        // Firedancer's 26.8.2 orders above agave's 4.2.0 numerically, and must not count here.
        assert_eq!(shares[&(EPOCH, "agave".to_string())]["4.2.0"], 0.0);
        assert_eq!(shares[&(EPOCH, "firedancer".to_string())]["26.8.2"], 0.0);
    }

    #[test]
    fn versions_order_by_number_rather_than_text() {
        // A text compare puts 0.812 above 0.1106 and would read this validator as current.
        let cluster = [
            validator(vec![client(
                EPOCH,
                Some("0.812.30108"),
                Some("frankendancer"),
                1,
            )]),
            validator(vec![client(
                EPOCH,
                Some("0.1106.40201"),
                Some("frankendancer"),
                9,
            )]),
        ];
        let shares = newer_stake_shares(&cluster);

        assert_eq!(
            shares[&(EPOCH, "frankendancer".to_string())]["0.812.30108"],
            0.9
        );
    }

    #[test]
    fn a_version_nothing_can_parse_weighs_on_neither_side() {
        let cluster = [
            validator(vec![agave(EPOCH, "4.1.0", 100)]),
            validator(vec![agave(EPOCH, "4.2.0", 100)]),
            validator(vec![agave(EPOCH, "not-a-version", 800)]),
        ];
        let shares = newer_stake_shares(&cluster);

        // 100 of the 200 stake that could be compared, not 100 of 1000.
        assert_eq!(shares[&(EPOCH, "agave".to_string())]["4.1.0"], 0.5);
    }

    #[test]
    fn a_client_the_registry_does_not_know_is_left_out() {
        let cluster = [
            validator(vec![agave(EPOCH, "4.2.0", 100)]),
            validator(vec![client(EPOCH, Some("4.1.0"), None, 900)]),
        ];
        let shares = newer_stake_shares(&cluster);

        assert!(!shares[&(EPOCH, "agave".to_string())].contains_key("4.1.0"));
    }

    /// A cluster that left 4.1.0 behind: 90% of agave stake sits on 4.2.0 every epoch.
    fn moved_on(epochs: std::ops::RangeInclusive<u64>) -> Vec<Vec<dto::ValidatorEpochStats>> {
        vec![epochs.map(|epoch| agave(epoch, "4.2.0", 900)).collect()]
    }

    #[test]
    fn one_epoch_behind_alone_is_forgiven() {
        let served = outdated_served(vec![agave(100, "4.1.0", 100)], moved_on(99..=101), 99..=101);

        assert!(served.is_empty());
    }

    #[test]
    fn a_second_epoch_behind_opens_the_incident() {
        let served = outdated_served(
            vec![agave(100, "4.1.0", 100), agave(101, "4.1.0", 100)],
            moved_on(99..=101),
            99..=101,
        );

        assert_eq!(served, vec![101]);
    }

    #[test]
    fn every_epoch_after_the_grace_is_its_own_record() {
        let subject = (100..=104)
            .map(|epoch| agave(epoch, "4.1.0", 100))
            .collect();
        let served = outdated_served(subject, moved_on(100..=104), 100..=104);

        assert_eq!(served, vec![101, 102, 103, 104]);
    }

    #[test]
    fn upgrading_ends_the_run() {
        let subject = vec![
            agave(100, "4.1.0", 100),
            agave(101, "4.1.0", 100),
            agave(102, "4.2.0", 100),
            agave(103, "4.2.0", 100),
        ];
        let served = outdated_served(subject, moved_on(100..=103), 100..=103);

        assert_eq!(served, vec![101]);
    }

    #[test]
    fn an_epoch_the_validator_reported_nothing_for_breaks_the_run() {
        // Nothing at 101, so 102 has no predecessor over the bar to clear it.
        let subject = vec![
            agave(100, "4.1.0", 100),
            agave(102, "4.1.0", 100),
            agave(103, "4.1.0", 100),
        ];
        let served = outdated_served(subject, moved_on(100..=103), 100..=103);

        assert_eq!(served, vec![103]);
    }

    #[test]
    fn a_validator_holding_a_fifth_of_its_lineage_can_never_fall_behind() {
        let subject = (100..=104)
            .map(|epoch| agave(epoch, "4.1.0", 250))
            .collect();
        let others = vec![(100..=104)
            .map(|epoch| agave(epoch, "4.2.0", 750))
            .collect()];

        assert!(outdated_served(subject, others, 100..=104).is_empty());
    }

    #[test]
    fn the_bar_is_read_at_eighty_percent() {
        let subject = vec![agave(100, "4.1.0", 200), agave(101, "4.1.0", 200)];
        let at_the_bar = vec![vec![agave(100, "4.2.0", 800), agave(101, "4.2.0", 800)]];
        let under_it = vec![vec![agave(100, "4.2.0", 799), agave(101, "4.2.0", 799)]];

        assert_eq!(
            outdated_served(subject.clone(), at_the_bar, 100..=101),
            vec![101]
        );
        assert!(outdated_served(subject, under_it, 100..=101).is_empty());
    }

    #[test]
    fn each_epoch_is_judged_in_the_lineage_it_ran() {
        // Behind on agave, then current on firedancer: the switch clears it.
        let subject = vec![
            agave(100, "4.1.0", 100),
            agave(101, "4.1.0", 100),
            client(102, Some("26.8.2"), Some("firedancer"), 100),
        ];
        let others = vec![
            (100..=102)
                .map(|epoch| agave(epoch, "4.2.0", 900))
                .collect(),
            vec![client(102, Some("26.8.2"), Some("firedancer"), 900)],
        ];

        assert_eq!(outdated_served(subject, others, 100..=102), vec![101]);
    }

    #[test]
    fn the_window_trims_the_records_but_not_the_predecessor_that_clears_them() {
        let subject = (100..=102)
            .map(|epoch| agave(epoch, "4.1.0", 100))
            .collect();

        // 101 is the window's first epoch, and 100 outside it still grants its grace.
        assert_eq!(
            outdated_served(subject, moved_on(100..=102), 101..=102),
            vec![101, 102]
        );
    }

    #[test]
    fn the_epoch_in_flight_is_not_judged() {
        let mut running = agave(101, "4.1.0", 100);
        running.epoch_end_at = None;
        let served = outdated_served(
            vec![agave(100, "4.1.0", 100), running],
            moved_on(100..=101),
            100..=101,
        );

        assert!(served.is_empty());
    }

    #[test]
    fn an_outdated_epoch_before_the_window_is_dropped() {
        let records = ValidatorIncidentRecords {
            outdated_clients: vec![outdated(99, 0.9), outdated(100, 0.9)],
            ..Default::default()
        };
        let filters = IncidentFilters {
            from_epoch: 100,
            ..Default::default()
        };

        let incidents = records.into_response_incidents(&filters);
        assert_eq!(
            incidents
                .iter()
                .map(|incident| incident.epoch)
                .collect::<Vec<_>>(),
            vec![100]
        );
    }

    #[test]
    fn a_validator_with_no_material_serves_nothing() {
        let incidents = ValidatorIncidents::default();

        assert!(incidents
            .into_response_incidents("voteA", &Default::default())
            .is_empty());
    }
}
