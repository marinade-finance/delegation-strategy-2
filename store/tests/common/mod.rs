#![allow(dead_code)]

use tokio_postgres::{Client, NoTls};

pub const POSTGRES_URL_ENV: &str = "DS_TEST_POSTGRES_URL";

pub async fn migrated_client(schema: &str) -> Option<Client> {
    let url = std::env::var(POSTGRES_URL_ENV).ok()?;

    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            panic!("postgres connection error: {err}");
        }
    });

    client
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE;
             CREATE SCHEMA {schema};
             SET search_path TO {schema}"
        ))
        .await
        .unwrap();

    let migrations_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../migrations");
    let mut migrations: Vec<_> = std::fs::read_dir(migrations_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
        .collect();
    migrations.sort();
    for migration in migrations {
        let sql = std::fs::read_to_string(&migration).unwrap();
        client
            .batch_execute(&sql)
            .await
            .unwrap_or_else(|err| panic!("migration {} failed: {err}", migration.display()));
    }

    Some(client)
}

pub fn skip_without_database(schema: &str) -> bool {
    if std::env::var(POSTGRES_URL_ENV).is_ok() {
        return false;
    }
    eprintln!("skipping {schema}: {POSTGRES_URL_ENV} is not set");
    true
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
