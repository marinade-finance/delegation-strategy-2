use crate::utils::order::{compare_keys, OrderDirection, OrderField, SortKey};
use rust_decimal::prelude::*;
use std::cmp::Ordering;
use std::collections::HashMap;
use store::dto::{ValidatorGroupNode, ValidatorGroupRecord, ValidatorGroupTree, ValidatorGroups};

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
pub struct GroupsPage {
    pub groups: Vec<ValidatorGroupRecord>,
    /// Number of groups matching the query, before `offset`/`limit`.
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
            |group: &ValidatorGroupRecord| SortKey::Number(Decimal::from(group.incident_count_3m))
        }
    }
}

pub fn group_column(group: &ValidatorGroupRecord, order_field: OrderField) -> SortKey {
    field_extractor(order_field)(group)
}

/// Orders two rows on their already-extracted column, then on their name.
pub fn compare_group_rows(
    (a_column, a_name): (&SortKey, &str),
    (b_column, b_name): (&SortKey, &str),
    order_direction: &OrderDirection,
) -> Ordering {
    compare_keys(a_column, b_column, order_direction).then_with(|| {
        a_name
            .to_lowercase()
            .cmp(&b_name.to_lowercase())
            .then_with(|| a_name.cmp(b_name))
    })
}

pub fn sort_groups(
    groups: Vec<ValidatorGroupRecord>,
    order_field: OrderField,
    order_direction: &OrderDirection,
) -> Vec<ValidatorGroupRecord> {
    // Keyed up front: sort_by would otherwise re-extract on both sides of every comparison.
    let mut keyed: Vec<(SortKey, ValidatorGroupRecord)> = groups
        .into_iter()
        .map(|group| (group_column(&group, order_field), group))
        .collect();

    keyed.sort_by(|(a_column, a), (b_column, b)| {
        compare_group_rows((a_column, &a.key), (b_column, &b.key), order_direction)
    });

    keyed.into_iter().map(|(_, group)| group).collect()
}

fn filter_groups(
    groups: Vec<ValidatorGroupRecord>,
    config: &GetGroupsConfig,
) -> Vec<ValidatorGroupRecord> {
    let Some(query) = search_term(config) else {
        return groups;
    };

    groups
        .into_iter()
        .filter(|group| group.key.to_lowercase().contains(&query))
        .collect()
}

fn search_term(config: &GetGroupsConfig) -> Option<String> {
    config
        .query
        .as_ref()
        .map(|query| query.trim().to_lowercase())
        .filter(|query| !query.is_empty())
}

pub fn page_groups(groups: ValidatorGroups, config: &GetGroupsConfig) -> GroupsPage {
    let ValidatorGroups {
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

    GroupsPage {
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
    use store::groups::UNKNOWN_GROUP;

    fn group(key: &str, stake: i64) -> ValidatorGroupRecord {
        ValidatorGroupRecord {
            key: key.to_string(),
            total_stake: Decimal::from(stake),
            ..Default::default()
        }
    }

    fn groups(groups: Vec<ValidatorGroupRecord>) -> ValidatorGroups {
        ValidatorGroups {
            total_activated_stake: groups.iter().map(|group| group.total_stake).sum(),
            groups,
            current_epoch: Some(100),
        }
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

    fn keys(page: &GroupsPage) -> Vec<String> {
        page.groups.iter().map(|group| group.key.clone()).collect()
    }

    fn named(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|key| key.to_string()).collect()
    }

    fn node(key: &str, stake: i64, children: Vec<ValidatorGroupRecord>) -> ValidatorGroupNode {
        ValidatorGroupNode {
            group: group(key, stake),
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
        let with_unknown = tree(vec![
            node(UNKNOWN_GROUP, 900, vec![group("Sonic", 900)]),
            node("Agave", 100, vec![group("Agave", 100)]),
        ]);
        let page = page_tree(
            with_unknown,
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
    }

    #[test]
    fn the_unclassified_group_is_searchable_by_name() {
        let page = page_tree(
            tree(vec![
                node("Agave", 700, vec![group("Agave", 700)]),
                node(UNKNOWN_GROUP, 300, vec![group(UNKNOWN_GROUP, 300)]),
            ]),
            &GetGroupsConfig {
                query: Some("unknown".to_string()),
                ..config()
            },
        );
        assert_eq!(parent_keys(&page), named(&[UNKNOWN_GROUP]));
        assert_eq!(page.total_count, 1);
    }

    #[test]
    fn stake_orders_both_ways() {
        let page = page_groups(
            groups(vec![
                group("mid", 200),
                group("low", 100),
                group("high", 300),
            ]),
            &config(),
        );
        assert_eq!(keys(&page), named(&["high", "mid", "low"]));

        let page = page_groups(
            groups(vec![
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
                groups(vec![
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
                groups(vec![
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
                incident_count_3m: 900,
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
        ] {
            let page = page_groups(
                groups(rows.clone()),
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

    fn providers() -> ValidatorGroups {
        groups(vec![
            group("Hetzner Online GmbH", 300),
            group("Latitude.sh", 200),
            group("TeraSwitch Networks Inc.", 100),
        ])
    }

    #[test]
    fn a_query_cuts_the_rows_and_the_count_but_not_the_totals() {
        let page = page_groups(
            providers(),
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
            providers(),
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
        let all = groups(vec![
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
