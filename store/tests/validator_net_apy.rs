use std::collections::HashMap;
use store::utils::load_validator_net_apy;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

async fn net_apy_stub(status_line: &'static str, body: &'static str) -> String {
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
                request_line.contains("/v1/rolling-apy/validator/latest/all"),
                "the stub answers the latest-net-APY endpoint only: {request_line}"
            );
            assert!(
                !request_line.contains("window="),
                "the window is fixed by apy-api, so this loader must not send one: {request_line}"
            );
            let response = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    format!("http://127.0.0.1:{port}")
}

#[tokio::test]
async fn the_apy_is_taken_at_full_precision_and_keyed_by_vote_account() {
    let base = net_apy_stub(
        "HTTP/1.1 200 OK",
        r#"{"voteOne":{"apy":0.07123456789,"time":1735689600},"voteTwo":{"apy":0.0712345,"time":1735689600}}"#,
    )
    .await;

    assert_eq!(
        load_validator_net_apy(&base).await.unwrap(),
        HashMap::from([
            ("voteOne".to_string(), 0.07123456789),
            ("voteTwo".to_string(), 0.0712345),
        ]),
        "values must arrive unrounded, otherwise near-equal validators tie and the sort looks broken"
    );
}

#[tokio::test]
async fn an_empty_answer_is_reported_as_an_answer() {
    let base = net_apy_stub("HTTP/1.1 200 OK", "{}").await;

    assert_eq!(
        load_validator_net_apy(&base).await.unwrap(),
        HashMap::new(),
        "an empty map is a successful answer; only the caller decides whether to act on it"
    );
}

#[tokio::test]
async fn a_failing_endpoint_is_an_error_not_an_empty_map() {
    let base = net_apy_stub("HTTP/1.1 503 Service Unavailable", "{}").await;

    assert!(
        load_validator_net_apy(&base).await.is_err(),
        "a failure must not be indistinguishable from apy-api knowing nobody"
    );
}
