use std::collections::HashSet;
use store::utils::load_protected_validators;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

async fn protected_stub(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(socket);
            let mut request_line = Vec::new();
            stream.read_until(b'\n', &mut request_line).await.unwrap();
            let request_line = String::from_utf8_lossy(&request_line).to_string();
            assert!(
                request_line.contains("/v1/validators/protected"),
                "the stub answers the protected-validators endpoint only: {request_line}"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    format!("http://127.0.0.1:{port}")
}

#[tokio::test]
async fn the_endpoints_list_is_passed_through_intact() {
    let base = protected_stub(r#"{"protected_validators":["voteOne","voteTwo"]}"#).await;

    assert_eq!(
        load_protected_validators(&base).await.unwrap(),
        HashSet::from(["voteOne".to_string(), "voteTwo".to_string()]),
        "the rule lives upstream now, so this loader must not filter or reshape the answer"
    );
}

#[tokio::test]
async fn an_empty_validator_list_is_reported_as_an_answer() {
    let base = protected_stub(r#"{"protected_validators":[]}"#).await;

    assert_eq!(
        load_protected_validators(&base).await.unwrap(),
        HashSet::new(),
        "an empty list is a successful answer; only the caller decides whether to act on it"
    );
}
