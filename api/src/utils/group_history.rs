use crate::cache::DEFAULT_CACHE_EPOCHS;
use crate::utils::response::response_error;
use chrono::{DateTime, Utc};
use serde::Serialize;
use store::dto::{GroupHistory, GroupHistoryEpoch};
use warp::{
    http::StatusCode,
    reply::{json, Json, WithStatus},
};

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct ResponseGroupHistory {
    name: String,
    current_epoch: Option<u64>,
    net_apy_updated_at: Option<DateTime<Utc>>,
    epochs: Vec<GroupHistoryEpoch>,
}

pub fn history_epochs(epochs: Option<usize>) -> Result<usize, String> {
    let max = DEFAULT_CACHE_EPOCHS as usize;
    match epochs.unwrap_or(max) {
        epochs if (1..=max).contains(&epochs) => Ok(epochs),
        _ => Err(format!("epochs must be between 1 and {max}")),
    }
}

fn group_history_response(
    mut history: GroupHistory,
    epochs: usize,
    net_apy_updated_at: Option<DateTime<Utc>>,
) -> ResponseGroupHistory {
    history.epochs.truncate(epochs);
    ResponseGroupHistory {
        current_epoch: history.epochs.first().map(|point| point.epoch),
        name: history.name,
        net_apy_updated_at,
        epochs: history.epochs,
    }
}

pub fn group_history_reply(
    history: Option<GroupHistory>,
    epochs: usize,
    net_apy_updated_at: Option<DateTime<Utc>>,
    not_found: impl FnOnce() -> String,
) -> WithStatus<Json> {
    match history {
        Some(history) => warp::reply::with_status(
            json(&group_history_response(history, epochs, net_apy_updated_at)),
            StatusCode::OK,
        ),
        None => response_error(StatusCode::NOT_FOUND, not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epochs_default_to_the_whole_cached_window() {
        assert_eq!(history_epochs(None), Ok(DEFAULT_CACHE_EPOCHS as usize));
        assert_eq!(history_epochs(Some(1)), Ok(1));
        assert_eq!(
            history_epochs(Some(DEFAULT_CACHE_EPOCHS as usize)),
            Ok(DEFAULT_CACHE_EPOCHS as usize)
        );
    }

    #[test]
    fn epochs_outside_the_cached_window_are_refused() {
        assert!(history_epochs(Some(0)).is_err());
        assert!(
            history_epochs(Some(DEFAULT_CACHE_EPOCHS as usize + 1)).is_err(),
            "the cache holds no epoch past its window"
        );
    }

    fn point(epoch: u64) -> GroupHistoryEpoch {
        GroupHistoryEpoch {
            epoch,
            epoch_start_at: None,
            epoch_end_at: None,
            total_stake: Default::default(),
            stake_share: 0.0,
            validator_count: 0,
            net_apy: None,
            take_rate: None,
        }
    }

    #[test]
    fn the_response_keeps_the_newest_epochs() {
        let history = GroupHistory {
            name: "OVH".to_string(),
            epochs: vec![point(100), point(99), point(98)],
        };
        let response = group_history_response(history, 2, None);

        assert_eq!(response.name, "OVH");
        assert_eq!(response.current_epoch, Some(100));
        assert_eq!(response.epochs, vec![point(100), point(99)]);
    }

    #[test]
    fn an_unknown_group_is_not_found() {
        let reply = group_history_reply(None, 90, None, || "No provider named x".to_string());
        assert_eq!(
            warp::Reply::into_response(reply).status(),
            StatusCode::NOT_FOUND
        );
    }
}
