#![allow(dead_code)]

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
        commission_max_observed: None,
        commission_min_observed: None,
        commission_advertised: None,
        commission_effective: None,
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
        self_stake: 0.into(),
        superminority: false,
        stake_to_become_superminority: 0.into(),
        credits: 0.into(),
        leader_slots: 0.into(),
        blocks_produced: 0.into(),
        skip_rate: 0f64,
        uptime_pct: None,
        uptime: None,
        downtime: None,
        updated_at: None,
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
