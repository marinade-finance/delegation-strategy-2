#![allow(dead_code)]

use store::dto::Validator;

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

/// The tests own their store: fake-gcs-server plus marinade-directory, both on
/// the host network, torn down when the handle drops.
pub const GCS_IMAGE: &str = "fsouza/fake-gcs-server:1.56.1";
pub const DIRECTORY_IMAGE: &str = "marinade-directory:v0.1.0";
const JWT_SECRET: &str = "delegation-strategy-test-secret-at-least-32b";
const BUCKET: &str = "delegation-strategy";

pub struct DirectoryStore {
    containers: Vec<String>,
    pub url: String,
    pub token: String,
}

impl DirectoryStore {
    pub fn client(&self) -> store::directory::Directory {
        store::directory::Directory::new(self.url.clone(), self.token.clone())
            .expect("directory client")
    }
}

impl Drop for DirectoryStore {
    fn drop(&mut self) {
        for container in &self.containers {
            let _ = std::process::Command::new("docker")
                .args(["rm", "-f", container])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

/// `None` — with the reason on stderr — where docker cannot run, which is the
/// contract the SQL harness had.
pub async fn directory_store(test: &str) -> Option<DirectoryStore> {
    if !docker_available() {
        eprintln!("skipping {test}: docker is not available");
        return None;
    }

    let suffix = format!("{test}-{}", std::process::id());
    let gcs_port = free_port();
    let directory_port = free_port();
    let gcs = format!("ds-test-gcs-{suffix}");
    let directory = format!("ds-test-directory-{suffix}");
    let store = DirectoryStore {
        containers: vec![gcs.clone(), directory.clone()],
        url: format!("http://localhost:{directory_port}"),
        token: mint_token(),
    };

    run_container(
        &gcs,
        &[],
        GCS_IMAGE,
        &[
            "-backend",
            "memory",
            "-scheme",
            "http",
            "-port",
            &gcs_port.to_string(),
            "-public-host",
            &format!("localhost:{gcs_port}"),
        ],
    );
    wait_for(&format!("http://localhost:{gcs_port}/storage/v1/b")).await;
    create_bucket(gcs_port).await;

    run_container(
        &directory,
        &[
            &format!("STORAGE_EMULATOR_HOST=localhost:{gcs_port}"),
            &format!("GCS_BUCKET={BUCKET}"),
            &format!("JWT_SECRET={JWT_SECRET}"),
            &format!("PORT={directory_port}"),
            "METRICS_PORT=0",
        ],
        DIRECTORY_IMAGE,
        &[],
    );
    wait_for(&format!("{}/ready", store.url)).await;

    Some(store)
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

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("free port")
        .local_addr()
        .expect("free port address")
        .port()
}

fn run_container(name: &str, env: &[&str], image: &str, args: &[&str]) {
    let mut command = std::process::Command::new("docker");
    command.args(["run", "-d", "--rm", "--name", name, "--network", "host"]);
    for entry in env {
        command.args(["-e", entry]);
    }
    command.arg(image);
    command.args(args);
    let output = command.output().expect("docker run");
    assert!(
        output.status.success(),
        "docker run {image} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn wait_for(url: &str) {
    let client = reqwest::Client::new();
    for _ in 0..150 {
        if let Ok(response) = client.get(url).send().await {
            if response.status().is_success() {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    panic!("{url} never became ready");
}

async fn create_bucket(gcs_port: u16) {
    let response = reqwest::Client::new()
        .post(format!(
            "http://localhost:{gcs_port}/storage/v1/b?project=delegation-strategy"
        ))
        .header("Content-Type", "application/json")
        .body(format!(
            "{{\"name\":\"{BUCKET}\",\"versioning\":{{\"enabled\":true}}}}"
        ))
        .send()
        .await
        .expect("create bucket");
    assert!(
        response.status().is_success(),
        "create bucket answered {}",
        response.status()
    );
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
