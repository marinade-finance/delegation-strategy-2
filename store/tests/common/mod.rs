#![allow(dead_code)]

use collect::validators::ValidatorSnapshot;
use collect::validators_performance::ValidatorPerformance;
use store::dto::Validator;
use testcontainers::core::wait::HttpWaitStrategy;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

/// A snapshot entry with nothing set but its identity: every fold-level test
/// sets the handful of fields it is about.
pub fn validator(vote_account: &str, epoch: u64) -> Validator {
    Validator {
        identity: format!("identity-{vote_account}"),
        vote_account: vote_account.to_string(),
        epoch: epoch.into(),
        info_name: None,
        info_url: None,
        info_keybase: None,
        info_icon_url: None,
        node_ip: None,
        dc_coordinates_lat: None,
        dc_coordinates_lon: None,
        dc_continent: None,
        dc_country_iso: None,
        dc_country: None,
        dc_city: None,
        dc_asn: None,
        dc_aso: None,
        dc_resolved: false,
        commission_max_observed: None,
        commission_min_observed: None,
        commission_advertised: None,
        commission_effective: None,
        commission_effective_source: None,
        commission_effective_bps: None,
        version: None,
        client_id: None,
        client_id_raw: None,
        feature_set: None,
        shred_version: None,
        gossip_port: None,
        rpc_public: None,
        pubsub_public: None,
        activated_stake: 0.into(),
        marinade_stake: 0.into(),
        foundation_stake: 0.into(),
        marinade_native_stake: 0.into(),
        institutional_stake: 0.into(),
        direct_stake: None,
        direct_activating_stake: None,
        direct_deactivating_stake: None,
        self_stake: 0.into(),
        activating_stake: None,
        deactivating_stake: None,
        superminority: false,
        stake_to_become_superminority: 0.into(),
        credits: Some(0.into()),
        vote_reward_lamports: None,
        leader_slots: 0.into(),
        blocks_produced: 0.into(),
        skip_rate: 0f64,
        uptime_pct: None,
        uptime: None,
        downtime: None,
        updated_at: None,
        inflation_rewards_collector: None,
        block_revenue_collector: None,
        inflation_rewards_commission_bps: None,
        inflation_rewards_commission_bps_is_v4: None,
        block_revenue_commission_bps: None,
        pending_delegator_rewards: None,
        inflation_rewards_collector_owner: None,
        inflation_rewards_collector_lamports: None,
        inflation_rewards_collector_healthy: None,
        block_revenue_collector_owner: None,
        block_revenue_collector_lamports: None,
        block_revenue_collector_healthy: None,
    }
}

pub fn validator_performance() -> ValidatorPerformance {
    ValidatorPerformance {
        commission: 7,
        version: Some("2.0.0".into()),
        client_id: None,
        client_id_raw: None,
        feature_set: None,
        shred_version: None,
        credits: Some(10),
        vote_reward_lamports: None,
        last_vote: Some(1),
        credits_total: Some(10),
        leader_slots: 100,
        blocks_produced: 100,
        skip_rate: 0f64,
        delinquent: false,
    }
}

/// Fixtures are written under one directory and removed by the test that wrote
/// them, so the name carries a counter: two tests naming the same snapshot run
/// in parallel threads and would otherwise delete each other's file.
pub fn write_yaml<T: serde::Serialize>(name: &str, snapshot: &T) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{nth}.yaml"));
    std::fs::write(&path, serde_yaml::to_string(snapshot).expect("yaml")).expect("snapshot file");
    path.to_str().expect("snapshot path").to_string()
}

/// One collected validator carrying only what the fields under test are read
/// against; every caller overrides the field its own assertions turn on.
pub fn validator_snapshot(
    identity: &str,
    vote_account: &str,
    performance: ValidatorPerformance,
) -> ValidatorSnapshot {
    ValidatorSnapshot {
        identity: identity.into(),
        vote_account: vote_account.into(),
        node_ip: None,
        gossip_port: None,
        rpc_public: None,
        pubsub_public: None,
        info_name: None,
        info_url: None,
        info_details: None,
        info_keybase: None,
        info_icon_url: None,
        data_center: None,
        activated_stake: 100,
        foundation_stake: 0,
        self_stake: 0,
        marinade_stake: 0,
        marinade_native_stake: 0,
        institutional_stake: 0,
        activating_stake: None,
        deactivating_stake: None,
        direct_stake: None,
        direct_activating_stake: None,
        direct_deactivating_stake: None,
        superminority: false,
        stake_to_become_superminority: 0,
        performance,
        inflation_rewards_collector: None,
        block_revenue_collector: None,
        inflation_rewards_commission_bps: None,
        inflation_rewards_commission_bps_is_v4: None,
        block_revenue_commission_bps: None,
        pending_delegator_rewards: None,
        inflation_rewards_collector_owner: None,
        inflation_rewards_collector_lamports: None,
        inflation_rewards_collector_healthy: None,
        block_revenue_collector_owner: None,
        block_revenue_collector_lamports: None,
        block_revenue_collector_healthy: None,
    }
}

/// One marinade-directory container per test, on its in-memory backend, torn
/// down when the handle drops. The mem backend answers the same six-call
/// contract a GCS/S3 bucket does, which is all the client speaks to.
pub const DIRECTORY_IMAGE: &str = "marinade-directory:test";
const JWT_SECRET: &str = "delegation-strategy-test-secret-at-least-32b";
const DIRECTORY_PORT: u16 = 3000;

pub struct DirectoryStore {
    _container: ContainerAsync<GenericImage>,
    pub url: String,
    pub token: String,
}

impl DirectoryStore {
    pub fn client(&self) -> store::directory::Directory {
        store::directory::Directory::new(self.url.clone(), self.token.clone())
            .expect("directory client")
    }
}

/// `None`, with the reason on stderr, where docker cannot run.
pub async fn directory_store(test: &str) -> Option<DirectoryStore> {
    if !docker_available() {
        eprintln!("skipping {test}: docker is not available");
        return None;
    }

    let container = GenericImage::new("marinade-directory", "test")
        .with_exposed_port(DIRECTORY_PORT.tcp())
        .with_wait_for(WaitFor::http(
            HttpWaitStrategy::new("/ready")
                .with_port(DIRECTORY_PORT.tcp())
                .with_expected_status_code(200u16),
        ))
        .with_env_var("MEM_BUCKET", test)
        .with_env_var("JWT_SECRET", JWT_SECRET)
        .with_env_var("PORT", DIRECTORY_PORT.to_string())
        .with_env_var("METRICS_PORT", "0")
        .start()
        .await
        .expect("start marinade-directory");

    let port = container
        .get_host_port_ipv4(DIRECTORY_PORT.tcp())
        .await
        .expect("mapped port");

    Some(DirectoryStore {
        url: format!("http://localhost:{port}"),
        token: mint_token(),
        _container: container,
    })
}

fn docker_available() -> bool {
    std::process::Command::new("docker")
        .arg("info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn mint_token() -> String {
    #[derive(serde::Serialize)]
    struct Claims {
        sub: String,
        grants: Vec<String>,
        exp: u64,
    }

    let claims = Claims {
        sub: "delegation-strategy-test".to_string(),
        // The leading slash is required: `validators/**` matches nothing.
        grants: vec![
            "/validators/**:rw".to_string(),
            "/scoring/**:rw".to_string(),
        ],
        exp: (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp() as u64,
    };
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .expect("mint token")
}
