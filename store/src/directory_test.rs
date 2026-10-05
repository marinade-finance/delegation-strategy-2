use crate::directory::{Directory, DirectoryError, Fetch, Precondition};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
struct Body {
    epoch: u64,
}

struct CannedServer {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl CannedServer {
    async fn start(response: &str) -> Self {
        Self::start_each(&[response]).await
    }

    async fn start_each(responses: &[&str]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("test port");
        let url = format!("http://{}", listener.local_addr().expect("test addr"));
        let requests: Arc<Mutex<Vec<String>>> = Default::default();
        tokio::spawn(answer(
            listener,
            responses.iter().map(|r| r.to_string()).collect(),
            Arc::clone(&requests),
        ));
        Self { url, requests }
    }

    fn client(&self) -> Directory {
        Directory::new(self.url.clone(), "test-token".to_string()).expect("test client")
    }

    fn request(&self) -> String {
        self.request_at(0)
    }

    fn request_at(&self, index: usize) -> String {
        self.requests.lock().expect("test requests")[index].clone()
    }

    fn requests(&self) -> usize {
        self.requests.lock().expect("test requests").len()
    }
}

async fn answer(listener: TcpListener, responses: Vec<String>, requests: Arc<Mutex<Vec<String>>>) {
    for response in responses {
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
        // Closing the connection is what stops the client reusing it, so the
        // next request arrives as the next accept.
        socket.shutdown().await.expect("test shutdown");
    }
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

fn entry_line(epoch: &str) -> String {
    format!(
        "{{\"path\":\"/validators/snapshot/{epoch}\",\"name\":\"{epoch}\",\"version\":\"1\",\
         \"etag\":\"\\\"{epoch}-1\\\"\",\"created_at\":\"2026-09-10T20:01:37.512018Z\"}}\n"
    )
}

#[tokio::test]
async fn list_reads_one_entry_per_ndjson_line() {
    let lines = entry_line("9") + &entry_line("750");
    let server = CannedServer::start(&reply("200 OK", "", &lines)).await;

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

#[tokio::test]
async fn list_follows_the_next_link_until_the_store_stops_sending_one() {
    let first = reply(
        "200 OK",
        "Link: </v1/validators/snapshot/*?cursor=9>; rel=\"next\"\r\n",
        &entry_line("9"),
    );
    let second = reply(
        "200 OK",
        "Link: </v1/validators/snapshot/*?cursor=750>; rel=\"next\"\r\n",
        &entry_line("750"),
    );
    let last = reply("200 OK", "", &entry_line("1000"));
    let server = CannedServer::start_each(&[&first, &second, &last]).await;

    let entries = server
        .client()
        .list("/validators/snapshot")
        .await
        .expect("list");

    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, ["9", "750", "1000"]);
    assert_eq!(server.requests(), 3);
    assert!(server
        .request_at(1)
        .starts_with("GET /v1/validators/snapshot/*?cursor=9 "));
    assert!(server
        .request_at(2)
        .starts_with("GET /v1/validators/snapshot/*?cursor=750 "));
}

#[tokio::test]
async fn resolve_reads_the_path_the_selector_landed_on() {
    let server = CannedServer::start(&reply(
        "200 OK",
        "Content-Location: /validators/snapshot/1049?v=7\r\n",
        "",
    ))
    .await;

    let path = server
        .client()
        .resolve("/validators/snapshot/@last")
        .await
        .expect("resolve");

    assert_eq!(path.as_deref(), Some("/validators/snapshot/1049"));
    assert!(server
        .request()
        .starts_with("HEAD /v1/validators/snapshot/@last "));
}

#[tokio::test]
async fn resolve_of_an_empty_collection_is_none() {
    let server = CannedServer::start(&reply("404 Not Found", "", "")).await;

    let path = server
        .client()
        .resolve("/validators/snapshot/@last")
        .await
        .expect("resolve");

    assert!(path.is_none());
}
