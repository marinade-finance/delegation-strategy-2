use crate::utils::order::{compare_keys, net_pending_stake, OrderDirection, OrderField, SortKey};
use rust_decimal::prelude::*;
use std::cmp::Ordering;
use std::collections::HashMap;
use store::dto::{
    GroupRow, ValidatorGroupNode, ValidatorGroupRecord, ValidatorGroupTree,
    ValidatorProviderGroupRecord, ValidatorProviderGroups,
};

pub const DEFAULT_LIMIT: usize = 100;

#[derive(Debug)]
pub struct GetGroupsConfig {
    pub order_field: OrderField,
    pub order_direction: OrderDirection,
    pub offset: usize,
    pub limit: usize,
    pub query: Option<String>,
}

#[derive(Debug)]
pub struct ProviderGroupsPage {
    pub groups: Vec<ValidatorProviderGroupRecord>,
    /// Number of providers matching the query, before `offset`/`limit`.
    pub total_count: usize,
    pub total_activated_stake: Decimal,
    pub current_epoch: Option<u64>,
}

type FieldExtractor = fn(&ValidatorGroupRecord) -> SortKey;

fn field_extractor(order_field: OrderField) -> FieldExtractor {
    match order_field {
        OrderField::Name => |group: &ValidatorGroupRecord| SortKey::Text(group.key.to_lowercase()),
        OrderField::Stake => |group: &ValidatorGroupRecord| SortKey::Number(group.total_stake),
        OrderField::StakeDelta7d => |group: &ValidatorGroupRecord| group.stake_delta_7d.into(),
        OrderField::StakeDelta30d => |group: &ValidatorGroupRecord| group.stake_delta_30d.into(),
        OrderField::ActivatingStake => |group: &ValidatorGroupRecord| group.activating_stake.into(),
        OrderField::NetPendingStake => |group: &ValidatorGroupRecord| {
            net_pending_stake(group.activating_stake, group.deactivating_stake)
        },
        OrderField::NetApy => {
            |group: &ValidatorGroupRecord| group.net_apy.and_then(Decimal::from_f64_retain).into()
        }
        OrderField::TakeRate => {
            |group: &ValidatorGroupRecord| group.take_rate.and_then(Decimal::from_f64_retain).into()
        }
        OrderField::Credits => {
            |group: &ValidatorGroupRecord| group.credits.and_then(Decimal::from_f64_retain).into()
        }
        OrderField::MarinadeScore => |group: &ValidatorGroupRecord| {
            group
                .marinade_score
                .and_then(Decimal::from_f64_retain)
                .into()
        },
        OrderField::Apy => {
            |group: &ValidatorGroupRecord| group.apy.and_then(Decimal::from_f64_retain).into()
        }
        OrderField::Commission => |group: &ValidatorGroupRecord| {
            group.commission.and_then(Decimal::from_f64_retain).into()
        },
        OrderField::Uptime => |group: &ValidatorGroupRecord| {
            group.uptime_pct.and_then(Decimal::from_f64_retain).into()
        },
        OrderField::ExpectedTakeRate => |group: &ValidatorGroupRecord| {
            group
                .expected_take_rate
                .and_then(Decimal::from_f64_retain)
                .into()
        },
        OrderField::Validators => {
            |group: &ValidatorGroupRecord| SortKey::Number(Decimal::from(group.validator_count))
        }
        OrderField::DelegationRelationships => |group: &ValidatorGroupRecord| {
            group
                .delegation_relationship_count
                .map(Decimal::from)
                .into()
        },
        OrderField::Incidents => {
            |group: &ValidatorGroupRecord| SortKey::Number(Decimal::from(group.incidents.count()))
        }
    }
}

fn secondary_field_extractor(order_field: OrderField) -> FieldExtractor {
    match order_field {
        // Breaks ties on `credits`, which is null for every group after the Alpenglow migration epoch.
        OrderField::Credits => |group: &ValidatorGroupRecord| {
            group
                .vote_reward_per_stake
                .and_then(Decimal::from_f64_retain)
                .into()
        },
        _ => |_: &ValidatorGroupRecord| SortKey::Missing,
    }
}

pub fn group_column(group: &ValidatorGroupRecord, order_field: OrderField) -> SortKey {
    field_extractor(order_field)(group)
}

pub fn group_secondary_column(group: &ValidatorGroupRecord, order_field: OrderField) -> SortKey {
    secondary_field_extractor(order_field)(group)
}

/// Orders two rows on their already-extracted columns, then on their name.
pub fn compare_group_rows(
    (a_primary, a_secondary, a_name): (&SortKey, &SortKey, &str),
    (b_primary, b_secondary, b_name): (&SortKey, &SortKey, &str),
    order_direction: &OrderDirection,
) -> Ordering {
    compare_keys(a_primary, b_primary, order_direction)
        .then_with(|| compare_keys(a_secondary, b_secondary, order_direction))
        .then_with(|| {
            a_name
                .to_lowercase()
                .cmp(&b_name.to_lowercase())
                .then_with(|| a_name.cmp(b_name))
        })
}

pub fn sort_groups<T: GroupRow>(
    groups: Vec<T>,
    order_field: OrderField,
    order_direction: &OrderDirection,
) -> Vec<T> {
    // Keyed up front: sort_by would otherwise re-extract on both sides of every comparison.
    let mut keyed: Vec<(SortKey, SortKey, T)> = groups
        .into_iter()
        .map(|group| {
            (
                group_column(group.row(), order_field),
                group_secondary_column(group.row(), order_field),
                group,
            )
        })
        .collect();

    keyed.sort_by(|(a_primary, a_secondary, a), (b_primary, b_secondary, b)| {
        compare_group_rows(
            (a_primary, a_secondary, &a.row().key),
            (b_primary, b_secondary, &b.row().key),
            order_direction,
        )
    });

    keyed.into_iter().map(|(.., group)| group).collect()
}

fn filter_groups<T: GroupRow>(groups: Vec<T>, config: &GetGroupsConfig) -> Vec<T> {
    let Some(query) = search_term(config) else {
        return groups;
    };

    groups
        .into_iter()
        .filter(|group| group.row().key.to_lowercase().contains(&query))
        .collect()
}

fn search_term(config: &GetGroupsConfig) -> Option<String> {
    config
        .query
        .as_ref()
        .map(|query| query.trim().to_lowercase())
        .filter(|query| !query.is_empty())
}

pub fn page_groups(
    groups: ValidatorProviderGroups,
    config: &GetGroupsConfig,
) -> ProviderGroupsPage {
    let ValidatorProviderGroups {
        groups,
        total_activated_stake,
        current_epoch,
    } = groups;

    let matching = filter_groups(groups, config);
    let total_count = matching.len();
    let page = sort_groups(matching, config.order_field, &config.order_direction)
        .into_iter()
        .skip(config.offset)
        .take(config.limit)
        .collect();

    ProviderGroupsPage {
        groups: page,
        total_count,
        total_activated_stake,
        current_epoch,
    }
}

#[derive(Debug)]
pub struct TreePage {
    pub nodes: Vec<ValidatorGroupNode>,
    /// Number of clients matching the query, before `offset`/`limit`; block engines are never paged.
    pub total_count: usize,
    pub total_activated_stake: Decimal,
    pub current_epoch: Option<u64>,
}

fn matches_query(node: &ValidatorGroupNode, query: &str) -> bool {
    let matches = |name: &str| name.to_lowercase().contains(query);

    matches(&node.group.key) || node.children.iter().any(|child| matches(&child.key))
}

pub fn page_tree(tree: ValidatorGroupTree, config: &GetGroupsConfig) -> TreePage {
    let ValidatorGroupTree {
        nodes,
        total_activated_stake,
        current_epoch,
    } = tree;

    let query = search_term(config);
    let matching: Vec<_> = nodes
        .into_iter()
        .filter(|node| match &query {
            None => true,
            Some(query) => matches_query(node, query),
        })
        .collect();
    let total_count = matching.len();

    let mut children_by_key: HashMap<String, Vec<ValidatorGroupRecord>> = matching
        .iter()
        .map(|node| (node.group.key.clone(), node.children.clone()))
        .collect();
    let parents = sort_groups(
        matching.into_iter().map(|node| node.group).collect(),
        config.order_field,
        &config.order_direction,
    );

    let nodes = parents
        .into_iter()
        .skip(config.offset)
        .take(config.limit)
        .map(|group| {
            let children = children_by_key.remove(&group.key).unwrap_or_default();
            ValidatorGroupNode {
                children: sort_groups(children, config.order_field, &config.order_direction),
                group,
            }
        })
        .collect();

    TreePage {
        nodes,
        total_count,
        total_activated_stake,
        current_epoch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use store::dto::{GroupIncidentRecord, GroupIncidents, IncidentRecord};
    use store::groups::UNKNOWN_GROUP;

    fn long_incident() -> GroupIncidentRecord {
        GroupIncidentRecord {
            validator: "vote".to_string(),
            incident: IncidentRecord::Downtime {
                epoch: 100,
                start_at: Utc::now(),
                end_at: Utc::now(),
                downtime_seconds: 600,
                block_production: None,
            },
        }
    }

    fn group(key: &str, stake: i64) -> ValidatorGroupRecord {
        ValidatorGroupRecord {
            key: key.to_string(),
            total_stake: Decimal::from(stake),
            ..Default::default()
        }
    }

    fn providers(groups: Vec<ValidatorGroupRecord>) -> ValidatorProviderGroups {
        ValidatorProviderGroups {
            total_activated_stake: groups.iter().map(|group| group.total_stake).sum(),
            groups: groups
                .into_iter()
                .map(|group| ValidatorProviderGroupRecord {
                    group,
                    ..Default::default()
                })
                .collect(),
            current_epoch: Some(100),
        }
    }

    #[test]
    fn credits_order_breaks_ties_on_vote_reward_per_stake() {
        let with_credits =
            |key: &str, credits: Option<f64>, rate: Option<f64>| ValidatorGroupRecord {
                credits,
                vote_reward_per_stake: rate,
                ..group(key, 100)
            };
        let sorted = sort_groups(
            vec![
                with_credits("missing", None, None),
                with_credits("low", Some(10.0), Some(0.005)),
                with_credits("migration_low", Some(20.0), Some(0.001)),
                with_credits("migration_high", Some(20.0), Some(0.009)),
            ],
            OrderField::Credits,
            &OrderDirection::DESC,
        );
        assert_eq!(
            sorted
                .iter()
                .map(|group| group.key.as_str())
                .collect::<Vec<_>>(),
            vec!["migration_high", "migration_low", "low", "missing"]
        );
    }

    fn config() -> GetGroupsConfig {
        GetGroupsConfig {
            order_field: crate::utils::order::DEFAULT_ORDER_FIELD,
            order_direction: crate::utils::order::DEFAULT_ORDER_DIRECTION,
            offset: 0,
            limit: DEFAULT_LIMIT,
            query: None,
        }
    }

    fn keys(page: &ProviderGroupsPage) -> Vec<String> {
        page.groups.iter().map(|group| group.key.clone()).collect()
    }

    fn named(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|key| key.to_string()).collect()
    }

    fn node(key: &str, stake: i64, children: Vec<ValidatorGroupRecord>) -> ValidatorGroupNode {
        ValidatorGroupNode {
            group: store::dto::ValidatorClientGroupRecord {
                group: group(key, stake),
                ..Default::default()
            },
            children,
        }
    }

    fn tree(nodes: Vec<ValidatorGroupNode>) -> ValidatorGroupTree {
        ValidatorGroupTree {
            total_activated_stake: nodes.iter().map(|node| node.group.total_stake).sum(),
            nodes,
            current_epoch: Some(100),
        }
    }

    fn parent_keys(page: &TreePage) -> Vec<String> {
        page.nodes
            .iter()
            .map(|node| node.group.key.clone())
            .collect()
    }

    fn child_keys(page: &TreePage, parent: &str) -> Vec<String> {
        page.nodes
            .iter()
            .find(|node| node.group.key == parent)
            .unwrap_or_else(|| panic!("no {parent} in {:?}", parent_keys(page)))
            .children
            .iter()
            .map(|child| child.key.clone())
            .collect()
    }

    fn client_tree() -> ValidatorGroupTree {
        tree(vec![
            node(
                "Agave",
                700,
                vec![
                    group("Agave + Jito", 400),
                    group("Agave", 100),
                    group("Agave + Rakurai", 200),
                ],
            ),
            node("Frankendancer", 200, vec![group("Frankendancer", 200)]),
            node("Firedancer", 100, vec![group("Firedancer", 100)]),
        ])
    }

    #[test]
    fn the_sort_column_orders_the_clients_and_their_block_engines_alike() {
        let page = page_tree(client_tree(), &config());
        assert_eq!(
            parent_keys(&page),
            named(&["Agave", "Frankendancer", "Firedancer"])
        );
        assert_eq!(
            child_keys(&page, "Agave"),
            named(&["Agave + Jito", "Agave + Rakurai", "Agave"]),
            "block engines follow the same column as their clients"
        );

        let page = page_tree(
            client_tree(),
            &GetGroupsConfig {
                order_direction: OrderDirection::ASC,
                ..config()
            },
        );
        assert_eq!(
            parent_keys(&page),
            named(&["Firedancer", "Frankendancer", "Agave"])
        );
        assert_eq!(
            child_keys(&page, "Agave"),
            named(&["Agave", "Agave + Rakurai", "Agave + Jito"]),
            "reversing the sort reverses the block engines too"
        );
    }

    #[test]
    fn ordering_by_name_orders_both_levels_by_name() {
        let page = page_tree(
            client_tree(),
            &GetGroupsConfig {
                order_field: OrderField::Name,
                order_direction: OrderDirection::ASC,
                ..config()
            },
        );
        assert_eq!(
            parent_keys(&page),
            named(&["Agave", "Firedancer", "Frankendancer"])
        );
        assert_eq!(
            child_keys(&page, "Agave"),
            named(&["Agave", "Agave + Jito", "Agave + Rakurai"])
        );
    }

    #[test]
    fn a_search_matching_a_block_engine_keeps_its_client_and_all_its_engines() {
        let page = page_tree(
            client_tree(),
            &GetGroupsConfig {
                query: Some("rakurai".to_string()),
                ..config()
            },
        );
        assert_eq!(parent_keys(&page), named(&["Agave"]));
        assert_eq!(page.total_count, 1);
        assert_eq!(
            child_keys(&page, "Agave").len(),
            3,
            "the row exists to show which block engines run the client"
        );
    }

    #[test]
    fn paging_cuts_clients_and_never_their_block_engines() {
        let page = page_tree(
            client_tree(),
            &GetGroupsConfig {
                offset: 1,
                limit: 1,
                ..config()
            },
        );
        assert_eq!(parent_keys(&page), named(&["Frankendancer"]));
        assert_eq!(page.total_count, 3, "the count describes the whole match");
        assert_eq!(child_keys(&page, "Frankendancer").len(), 1);
    }

    #[test]
    fn the_unclassified_client_is_a_row_like_any_other() {
        let with_unknown = || {
            tree(vec![
                node(UNKNOWN_GROUP, 900, vec![group("Sonic", 900)]),
                node("Agave", 100, vec![group("Agave", 100)]),
            ])
        };
        let page = page_tree(
            with_unknown(),
            &GetGroupsConfig {
                order_field: OrderField::Name,
                order_direction: OrderDirection::ASC,
                ..config()
            },
        );
        assert_eq!(
            parent_keys(&page),
            named(&["Agave", UNKNOWN_GROUP]),
            "it carries a name, so it sorts by it rather than being special-cased"
        );
        assert_eq!(
            page.nodes[1].children.len(),
            1,
            "its block engines are still served"
        );

        // The only client here whose name no block engine of its own repeats.
        let searched = page_tree(
            with_unknown(),
            &GetGroupsConfig {
                query: Some(UNKNOWN_GROUP.to_string()),
                ..config()
            },
        );
        assert_eq!(
            parent_keys(&searched),
            named(&[UNKNOWN_GROUP]),
            "a search reads the client's own name, not only its block engines'"
        );
    }

    #[test]
    fn stake_orders_both_ways() {
        let page = page_groups(
            providers(vec![
                group("mid", 200),
                group("low", 100),
                group("high", 300),
            ]),
            &config(),
        );
        assert_eq!(keys(&page), named(&["high", "mid", "low"]));

        let page = page_groups(
            providers(vec![
                group("mid", 200),
                group("low", 100),
                group("high", 300),
            ]),
            &GetGroupsConfig {
                order_direction: OrderDirection::ASC,
                ..config()
            },
        );
        assert_eq!(keys(&page), named(&["low", "mid", "high"]));
    }

    fn with_net_apy(key: &str, net_apy: Option<f64>) -> ValidatorGroupRecord {
        ValidatorGroupRecord {
            net_apy,
            ..group(key, 100)
        }
    }

    #[test]
    fn groups_without_a_value_sink_in_both_directions() {
        for order_direction in [OrderDirection::ASC, OrderDirection::DESC] {
            let page = page_groups(
                providers(vec![
                    with_net_apy("aaaMissing", None),
                    with_net_apy("zero", Some(0.0)),
                    with_net_apy("high", Some(0.09)),
                ]),
                &GetGroupsConfig {
                    order_field: OrderField::NetApy,
                    order_direction,
                    ..config()
                },
            );
            assert_eq!(
                keys(&page).last().unwrap(),
                &"aaaMissing".to_string(),
                "no value must not read as the lowest rate"
            );
        }
    }

    #[test]
    fn equal_values_tiebreak_on_the_key_whichever_way_the_sort_runs() {
        for order_direction in [OrderDirection::ASC, OrderDirection::DESC] {
            let config = GetGroupsConfig {
                order_direction,
                ..config()
            };
            let page = page_groups(
                providers(vec![
                    group("ccc", 100),
                    group("aaa", 100),
                    group("bbb", 100),
                ]),
                &config,
            );
            assert_eq!(keys(&page), named(&["aaa", "bbb", "ccc"]), "{config:?}");
        }
    }

    #[test]
    fn every_sort_column_reads_its_own_field() {
        let rows = vec![
            ValidatorGroupRecord {
                total_stake: Decimal::from(900),
                ..group("stake", 100)
            },
            ValidatorGroupRecord {
                stake_delta_7d: Some(Decimal::from(900)),
                ..group("delta7d", 100)
            },
            ValidatorGroupRecord {
                stake_delta_30d: Some(Decimal::from(900)),
                ..group("delta30d", 100)
            },
            ValidatorGroupRecord {
                net_apy: Some(0.9),
                ..group("netApy", 100)
            },
            ValidatorGroupRecord {
                take_rate: Some(0.9),
                ..group("takeRate", 100)
            },
            ValidatorGroupRecord {
                validator_count: 900,
                ..group("validators", 100)
            },
            ValidatorGroupRecord {
                delegation_relationship_count: Some(900),
                ..group("relationships", 100)
            },
            ValidatorGroupRecord {
                incidents: GroupIncidents::Records(vec![long_incident(); 900]),
                ..group("incidents", 100)
            },
            ValidatorGroupRecord {
                credits: Some(0.9),
                ..group("credits", 100)
            },
            ValidatorGroupRecord {
                marinade_score: Some(0.9),
                ..group("marinadeScore", 100)
            },
            ValidatorGroupRecord {
                apy: Some(0.9),
                ..group("apy", 100)
            },
            ValidatorGroupRecord {
                commission: Some(0.9),
                ..group("commission", 100)
            },
            ValidatorGroupRecord {
                uptime_pct: Some(0.9),
                ..group("uptime", 100)
            },
            ValidatorGroupRecord {
                expected_take_rate: Some(0.9),
                ..group("expectedTakeRate", 100)
            },
            ValidatorGroupRecord {
                activating_stake: Some(Decimal::from(900)),
                deactivating_stake: Some(Decimal::from(900)),
                ..group("activatingStake", 100)
            },
            ValidatorGroupRecord {
                activating_stake: Some(Decimal::from(500)),
                deactivating_stake: Some(Decimal::ZERO),
                ..group("netPendingStake", 100)
            },
        ];

        for (order_field, leader) in [
            (OrderField::Stake, "stake"),
            (OrderField::StakeDelta7d, "delta7d"),
            (OrderField::StakeDelta30d, "delta30d"),
            (OrderField::NetApy, "netApy"),
            (OrderField::TakeRate, "takeRate"),
            (OrderField::Validators, "validators"),
            (OrderField::DelegationRelationships, "relationships"),
            (OrderField::Incidents, "incidents"),
            (OrderField::Credits, "credits"),
            (OrderField::MarinadeScore, "marinadeScore"),
            (OrderField::Apy, "apy"),
            (OrderField::Commission, "commission"),
            (OrderField::Uptime, "uptime"),
            (OrderField::ExpectedTakeRate, "expectedTakeRate"),
            (OrderField::ActivatingStake, "activatingStake"),
            (OrderField::NetPendingStake, "netPendingStake"),
        ] {
            let page = page_groups(
                providers(rows.clone()),
                &GetGroupsConfig {
                    order_field,
                    ..config()
                },
            );
            assert_eq!(
                keys(&page).first(),
                Some(&leader.to_string()),
                "{order_field:?} must lead with the row holding that field"
            );
        }
    }

    // `/validators` operator rows carry the records, `/clients` and `/providers` only the count;
    // both are paged by the same sort, so the column has to read the same quantity off either.
    #[test]
    fn rows_carrying_records_and_rows_carrying_a_count_order_against_each_other() {
        let page = page_groups(
            providers(vec![
                ValidatorGroupRecord {
                    incidents: GroupIncidents::Count(2),
                    ..group("countedTwo", 100)
                },
                ValidatorGroupRecord {
                    incidents: GroupIncidents::Records(vec![long_incident(); 3]),
                    ..group("listedThree", 100)
                },
                ValidatorGroupRecord {
                    incidents: GroupIncidents::Count(9),
                    ..group("countedNine", 100)
                },
                ValidatorGroupRecord {
                    incidents: GroupIncidents::Records(vec![long_incident()]),
                    ..group("listedOne", 100)
                },
            ]),
            &GetGroupsConfig {
                order_field: OrderField::Incidents,
                ..config()
            },
        );
        assert_eq!(
            keys(&page),
            named(&["countedNine", "listedThree", "countedTwo", "listedOne"])
        );
    }

    #[test]
    fn a_query_cuts_the_rows_and_the_count_but_not_the_totals() {
        let page = page_groups(
            providers(vec![
                group("Hetzner Online GmbH", 300),
                group("Latitude.sh", 200),
                group("TeraSwitch Networks Inc.", 100),
            ]),
            &GetGroupsConfig {
                query: Some("HETZ".to_string()),
                ..config()
            },
        );
        assert_eq!(keys(&page), named(&["Hetzner Online GmbH"]));
        assert_eq!(page.total_count, 1);
        assert_eq!(
            page.total_activated_stake,
            Decimal::from(600),
            "the stake total describes the whole set, not the match"
        );
    }

    #[test]
    fn a_query_of_only_whitespace_serves_every_row() {
        let page = page_groups(
            providers(vec![
                group("Hetzner Online GmbH", 300),
                group("Latitude.sh", 200),
                group("TeraSwitch Networks Inc.", 100),
            ]),
            &GetGroupsConfig {
                query: Some("  ".to_string()),
                ..config()
            },
        );
        assert_eq!(page.total_count, 3);
        assert_eq!(
            keys(&page),
            named(&[
                "Hetzner Online GmbH",
                "Latitude.sh",
                "TeraSwitch Networks Inc."
            ])
        );
    }

    #[test]
    fn paging_cuts_the_page_but_not_the_count() {
        let all = providers(vec![
            group("a", 500),
            group("b", 400),
            group("c", 300),
            group("d", 200),
        ]);
        let page = page_groups(
            all,
            &GetGroupsConfig {
                offset: 1,
                limit: 2,
                ..config()
            },
        );
        assert_eq!(keys(&page), named(&["b", "c"]));
        assert_eq!(page.total_count, 4);
    }
}
