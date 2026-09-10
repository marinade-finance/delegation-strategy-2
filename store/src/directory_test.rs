use crate::directory::{Directory, DirectoryError, Fetch, Precondition};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
struct Body {
    epoch: u64,
}

/// Answers one connection with a canned response and keeps the request head.
struct CannedServer {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl CannedServer {
    async fn start(response: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("test port");
        let url = format!("http://{}", listener.local_addr().expect("test addr"));
        let requests: Arc<Mutex<Vec<String>>> = Default::default();
        tokio::spawn(answer(
            listener,
            response.to_string(),
            Arc::clone(&requests),
        ));
        Self { url, requests }
    }

    fn client(&self) -> Directory {
        Directory::new(self.url.clone(), "test-token".to_string()).expect("test client")
    }

    fn request(&self) -> String {
        self.requests.lock().expect("test requests")[0].clone()
    }
}

async fn answer(listener: TcpListener, response: String, requests: Arc<Mutex<Vec<String>>>) {
    let (mut socket, _) = listener.accept().await.expect("test connection");
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if socket.read(&mut byte).await.expect("test request") == 0 {
            break;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).to_string();
    // Draining the body keeps the client from seeing a reset before it reads the reply.
    if let Some(length) = content_length(&head) {
        let mut body = vec![0u8; length];
        socket.read_exact(&mut body).await.expect("test body");
    }
    requests.lock().expect("test requests").push(head);
    socket
        .write_all(response.as_bytes())
        .await
        .expect("test reply");
    socket.shutdown().await.expect("test shutdown");
}

fn content_length(head: &str) -> Option<usize> {
    head.lines()
        .find(|line| line.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|line| line.split(':').nth(1))
        .and_then(|value| value.trim().parse().ok())
}

fn reply(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn get_of_a_missing_path_is_none() {
    let server = CannedServer::start(&reply("404 Not Found", "", "{}")).await;

    let doc = server
        .client()
        .get::<Body>("/validators/snapshot/750")
        .await
        .expect("get");

    assert!(doc.is_none());
}

#[tokio::test]
async fn get_reads_the_body_and_the_etag() {
    // The store spells the header `Etag`, so the client must not match case.
    let server =
        CannedServer::start(&reply("200 OK", "Etag: \"abc-1\"\r\n", "{\"epoch\":750}")).await;

    let doc = server
        .client()
        .get::<Body>("/validators/snapshot/750")
        .await
        .expect("get")
        .expect("document");

    assert_eq!(doc.body, Body { epoch: 750 });
    assert_eq!(doc.etag, "\"abc-1\"");
    assert!(server
        .request()
        .contains("authorization: Bearer test-token"));
}

#[tokio::test]
async fn conditional_get_reports_not_modified() {
    let server = CannedServer::start("HTTP/1.1 304 Not Modified\r\n\r\n").await;

    let fetch = server
        .client()
        .get_if_none_match::<Body>("/validators/live/uptimes", "\"abc-1\"")
        .await
        .expect("conditional get");

    assert!(matches!(fetch, Fetch::NotModified));
    assert!(server.request().contains("if-none-match: \"abc-1\""));
}

#[tokio::test]
async fn conditional_get_reports_a_missing_document() {
    let server = CannedServer::start(&reply("404 Not Found", "", "{}")).await;

    let fetch = server
        .client()
        .get_if_none_match::<Body>("/validators/live/uptimes", "\"abc-1\"")
        .await
        .expect("conditional get");

    assert!(matches!(fetch, Fetch::Missing));
}

#[tokio::test]
async fn create_asks_for_a_missing_document_and_returns_the_new_etag() {
    let server = CannedServer::start(&reply("201 Created", "Etag: \"abc-2\"\r\n", "")).await;

    let etag = server
        .client()
        .put(
            "/validators/snapshot/750",
            &Body { epoch: 750 },
            Precondition::Create,
        )
        .await
        .expect("create");

    assert_eq!(etag, "\"abc-2\"");
    assert!(server.request().contains("if-none-match: *"));
}

#[tokio::test]
async fn a_stale_write_is_a_conflict() {
    let server = CannedServer::start(&reply("412 Precondition Failed", "", "{}")).await;

    let error = server
        .client()
        .put(
            "/validators/live/uptimes",
            &Body { epoch: 750 },
            Precondition::IfMatch("\"abc-1\"".to_string()),
        )
        .await
        .expect_err("conflict");

    assert!(error.is_conflict(), "{error}");
    assert!(matches!(error, DirectoryError::Conflict(path) if path == "/validators/live/uptimes"));
    assert!(server.request().contains("if-match: \"abc-1\""));
}

#[tokio::test]
async fn an_unexpected_status_carries_it() {
    let server = CannedServer::start(&reply("403 Forbidden", "", "no grant")).await;

    let error = server
        .client()
        .get::<Body>("/validators/snapshot/750")
        .await
        .expect_err("forbidden");

    assert!(!error.is_conflict());
    assert!(format!("{error}").contains("403"), "{error}");
}

#[tokio::test]
async fn list_reads_one_entry_per_ndjson_line() {
    let lines = "{\"path\":\"/validators/snapshot/9\",\"name\":\"9\",\"version\":\"1\",\"etag\":\"\\\"a-1\\\"\",\"created_at\":\"2026-09-10T20:01:37.512018Z\"}\n\
                 {\"path\":\"/validators/snapshot/750\",\"name\":\"750\",\"version\":\"2\",\"etag\":\"\\\"b-2\\\"\",\"created_at\":\"2026-09-10T20:01:38.512018Z\"}\n";
    let server = CannedServer::start(&reply("200 OK", "", lines)).await;

    let entries = server
        .client()
        .list("/validators/snapshot")
        .await
        .expect("list");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "9");
    assert_eq!(entries[1].name, "750");
    assert!(server
        .request()
        .starts_with("GET /v1/validators/snapshot/* "));
}
