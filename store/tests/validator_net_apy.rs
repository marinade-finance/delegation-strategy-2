use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use store::utils::{load_validator_net_apy, load_validator_net_apy_history};
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

type HistoryAnswer = Arc<dyn Fn(usize, &[String]) -> (&'static str, String) + Send + Sync>;

async fn net_apy_history_stub(answer: HistoryAnswer) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(socket);
            let mut request_line = Vec::new();
            stream.read_until(b'\n', &mut request_line).await.unwrap();
            let request_line = String::from_utf8_lossy(&request_line).to_string();
            let validators: Vec<String> = request_line
                .split("validators=")
                .nth(1)
                .and_then(|rest| rest.split(['&', ' ']).next())
                .unwrap_or_default()
                .split(',')
                .map(str::to_string)
                .collect();
            let index = {
                let mut seen = seen.lock().unwrap();
                seen.push(request_line);
                seen.len() - 1
            };
            let (status_line, body) = answer(index, &validators);
            let response = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (format!("http://127.0.0.1:{port}"), requests)
}

fn series_body(validators: &[String], series: impl Fn(&str) -> String) -> String {
    let entries: Vec<String> = validators
        .iter()
        .map(|vote_account| format!(r#""{vote_account}":{}"#, series(vote_account)))
        .collect();
    format!("{{{}}}", entries.join(","))
}

fn vote_accounts(count: usize) -> Vec<String> {
    (0..count).rev().map(|i| format!("vote{i:03}")).collect()
}

#[tokio::test]
async fn history_is_fetched_in_chunks_and_merged() {
    let (base, requests) = net_apy_history_stub(Arc::new(|_, validators| {
        let body = series_body(validators, |vote_account| match vote_account {
            "vote000" => r#"{"times":[],"values":[],"labels":[]}"#.to_string(),
            _ => r#"{"times":[1000,2000],"values":[0.07,0.071],"labels":[]}"#.to_string(),
        });
        ("HTTP/1.1 200 OK", body)
    }))
    .await;

    let history = load_validator_net_apy_history(&base, &vote_accounts(151), 1234)
        .await
        .unwrap();

    assert_eq!(
        history.len(),
        150,
        "every validator with points is kept, the one apy-api has nothing for is left out"
    );
    assert!(!history.contains_key("vote000"));
    assert_eq!(history["vote150"], vec![(1000, 0.07), (2000, 0.071)]);

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "151 validators take two batch calls");
    let mut requested: Vec<String> = Vec::new();
    for request in requests.iter() {
        assert!(
            request.contains("/v1/rolling-apy/validator/batch?"),
            "{request}"
        );
        assert!(request.contains("window=1209600"), "{request}");
        assert!(request.contains("from=1234"), "{request}");
        let validators: Vec<String> = request
            .split("validators=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .split(',')
            .map(str::to_string)
            .collect();
        assert!(validators.len() <= 150, "{} validators", validators.len());
        requested.extend(validators);
    }
    let mut expected = vote_accounts(151);
    expected.sort();
    assert_eq!(
        requested, expected,
        "each validator is asked for once, sorted"
    );
}

#[tokio::test]
async fn a_failing_chunk_fails_the_whole_history() {
    let (base, _) = net_apy_history_stub(Arc::new(|index, validators| match index {
        0 => (
            "HTTP/1.1 200 OK",
            series_body(validators, |_| {
                r#"{"times":[1000],"values":[0.07],"labels":[]}"#.to_string()
            }),
        ),
        _ => ("HTTP/1.1 503 Service Unavailable", "{}".to_string()),
    }))
    .await;

    assert!(
        load_validator_net_apy_history(&base, &vote_accounts(151), 1234)
            .await
            .is_err(),
        "a partial snapshot must not replace a complete one"
    );
}

#[tokio::test]
async fn history_with_no_points_at_all_is_an_error() {
    let (base, _) = net_apy_history_stub(Arc::new(|_, validators| {
        (
            "HTTP/1.1 200 OK",
            series_body(validators, |_| {
                r#"{"times":[],"values":[],"labels":[]}"#.to_string()
            }),
        )
    }))
    .await;

    assert!(
        load_validator_net_apy_history(&base, &vote_accounts(3), 1234)
            .await
            .is_err(),
        "apy-api knowing nobody is more likely an upstream fault than the truth"
    );
}
