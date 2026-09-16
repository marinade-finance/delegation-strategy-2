use chrono::{DateTime, Utc};
use log::debug;
use reqwest::header::{CONTENT_LOCATION, CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH, LINK};
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// A document and the version to hand back in the next conditional write.
#[derive(Debug, Clone)]
pub struct Doc<T> {
    pub body: T,
    pub etag: String,
}

#[derive(Debug)]
pub enum Fetch<T> {
    Modified(Doc<T>),
    NotModified,
    Missing,
}

/// The store refuses a write without one, so every caller states its intent.
#[derive(Debug, Clone)]
pub enum Precondition {
    Create,
    IfMatch(String),
}

/// One line of a `GET /v1/{dir}/*` listing.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Entry {
    pub path: String,
    pub name: String,
    pub version: String,
    pub etag: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug)]
pub enum DirectoryError {
    /// 412: `Create` found a live document, or `IfMatch` lost the race.
    Conflict(String),
    Unexpected {
        path: String,
        status: StatusCode,
        body: String,
    },
    Transport {
        path: String,
        source: reqwest::Error,
    },
    Json {
        path: String,
        source: serde_json::Error,
    },
    NoEtag(String),
    NoLocation(String),
}

impl std::fmt::Display for DirectoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict(path) => write!(
                f,
                "Conflict on {path}: the stored version is not the one written against"
            ),
            Self::Unexpected { path, status, body } => {
                write!(f, "Request to {path} answered {status}: {body}")
            }
            Self::Transport { path, source } => write!(f, "Request to {path} failed: {source}"),
            Self::Json { path, source } => {
                write!(f, "Body of {path} is not the expected JSON: {source}")
            }
            Self::NoEtag(path) => write!(f, "Reply for {path} carries no ETag"),
            Self::NoLocation(path) => {
                write!(f, "Reply for {path} carries no Content-Location")
            }
        }
    }
}

impl std::error::Error for DirectoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl DirectoryError {
    pub fn is_conflict(&self) -> bool {
        matches!(self, Self::Conflict(_))
    }
}

/// HTTP client for marinade-directory, the versioned JSON document store.
#[derive(Debug, Clone)]
pub struct Directory {
    url: String,
    token: String,
    http: reqwest::Client,
}

impl Directory {
    pub fn new(url: String, token: String) -> Result<Self, reqwest::Error> {
        Ok(Self {
            url: url.trim_end_matches('/').to_string(),
            token,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()?,
        })
    }

    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<Option<Doc<T>>, DirectoryError> {
        let response = self
            .send(self.request(reqwest::Method::GET, path), path)
            .await?;
        match response.status() {
            StatusCode::OK => Ok(Some(self.read_doc(response, path).await?)),
            StatusCode::NOT_FOUND => Ok(None),
            _ => Err(self.unexpected(response, path).await),
        }
    }

    pub async fn get_if_none_match<T: DeserializeOwned>(
        &self,
        path: &str,
        etag: &str,
    ) -> Result<Fetch<T>, DirectoryError> {
        let request = self
            .request(reqwest::Method::GET, path)
            .header(IF_NONE_MATCH, etag);
        let response = self.send(request, path).await?;
        match response.status() {
            StatusCode::OK => Ok(Fetch::Modified(self.read_doc(response, path).await?)),
            StatusCode::NOT_MODIFIED => Ok(Fetch::NotModified),
            StatusCode::NOT_FOUND => Ok(Fetch::Missing),
            _ => Err(self.unexpected(response, path).await),
        }
    }

    pub async fn put<T: Serialize>(
        &self,
        path: &str,
        body: &T,
        precondition: Precondition,
    ) -> Result<String, DirectoryError> {
        let payload = serde_json::to_vec(body).map_err(|source| DirectoryError::Json {
            path: path.to_string(),
            source,
        })?;
        let request = self
            .request(reqwest::Method::PUT, path)
            .header(CONTENT_TYPE, "application/json")
            .body(payload);
        let request = match &precondition {
            Precondition::Create => request.header(IF_NONE_MATCH, "*"),
            Precondition::IfMatch(etag) => request.header(IF_MATCH, etag.as_str()),
        };

        let response = self.send(request, path).await?;
        match response.status() {
            StatusCode::OK | StatusCode::CREATED => {
                let etag =
                    read_etag(&response).ok_or_else(|| DirectoryError::NoEtag(path.to_string()))?;
                debug!("wrote {path} at {etag}");
                Ok(etag)
            }
            StatusCode::PRECONDITION_FAILED => Err(DirectoryError::Conflict(path.to_string())),
            _ => Err(self.unexpected(response, path).await),
        }
    }

    /// Every child, one level deep, in the store's natural order
    /// (`9 < 10 < 750`). The store answers a bounded page and names the one
    /// after it in `Link: rel="next"`; a caller that stops at the first page
    /// sees only the oldest children.
    pub async fn list(&self, dir: &str) -> Result<Vec<Entry>, DirectoryError> {
        let path = format!("{}/*", dir.trim_end_matches('/'));
        let mut url = self.url_for(&path);
        let mut entries = Vec::new();
        loop {
            let response = self.send(self.at(reqwest::Method::GET, url), &path).await?;
            if response.status() != StatusCode::OK {
                return Err(self.unexpected(response, &path).await);
            }
            let next = next_page(&response);
            let body = self.text(response, &path).await?;
            for line in body.lines().filter(|line| !line.trim().is_empty()) {
                entries.push(serde_json::from_str(line).map_err(|source| {
                    DirectoryError::Json {
                        path: path.clone(),
                        source,
                    }
                })?);
            }
            let Some(link) = next else {
                return Ok(entries);
            };
            url = format!("{}{link}", self.url);
        }
    }

    /// The path an `@last`/`@first` selector resolves to, or `None` where the
    /// collection holds nothing. The store resolves it against the whole
    /// collection, so unlike a listing it cannot be cut short by a page. HEAD
    /// because only the resolved name is wanted, never the document.
    pub async fn resolve(&self, path: &str) -> Result<Option<String>, DirectoryError> {
        let response = self
            .send(self.request(reqwest::Method::HEAD, path), path)
            .await?;
        match response.status() {
            StatusCode::OK => {
                let location = response
                    .headers()
                    .get(CONTENT_LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| DirectoryError::NoLocation(path.to_string()))?;
                // The header carries the version the read answered from.
                let resolved = location.split('?').next().unwrap_or(location);
                Ok(Some(resolved.to_string()))
            }
            StatusCode::NOT_FOUND => Ok(None),
            _ => Err(self.unexpected(response, path).await),
        }
    }

    /// Readiness probe; the only endpoint that takes no token.
    pub async fn ready(&self) -> Result<(), DirectoryError> {
        let path = "/ready";
        let url = format!("{}{path}", self.url);
        let response = self.send(self.http.get(url), path).await?;
        if response.status() == StatusCode::OK {
            return Ok(());
        }
        Err(self.unexpected(response, path).await)
    }

    fn request(&self, method: reqwest::Method, path: &str) -> RequestBuilder {
        self.at(method, self.url_for(path))
    }

    fn url_for(&self, path: &str) -> String {
        format!("{}/v1/{}", self.url, path.trim_start_matches('/'))
    }

    /// A page after the first is addressed by the URL the store handed back,
    /// which already carries the `/v1` prefix.
    fn at(&self, method: reqwest::Method, url: String) -> RequestBuilder {
        self.http.request(method, url).bearer_auth(&self.token)
    }

    async fn send(&self, request: RequestBuilder, path: &str) -> Result<Response, DirectoryError> {
        request
            .send()
            .await
            .map_err(|source| DirectoryError::Transport {
                path: path.to_string(),
                source,
            })
    }

    async fn read_doc<T: DeserializeOwned>(
        &self,
        response: Response,
        path: &str,
    ) -> Result<Doc<T>, DirectoryError> {
        let etag = read_etag(&response).ok_or_else(|| DirectoryError::NoEtag(path.to_string()))?;
        let bytes = response
            .bytes()
            .await
            .map_err(|source| DirectoryError::Transport {
                path: path.to_string(),
                source,
            })?;
        let body = serde_json::from_slice(&bytes).map_err(|source| DirectoryError::Json {
            path: path.to_string(),
            source,
        })?;
        Ok(Doc { body, etag })
    }

    async fn text(&self, response: Response, path: &str) -> Result<String, DirectoryError> {
        response
            .text()
            .await
            .map_err(|source| DirectoryError::Transport {
                path: path.to_string(),
                source,
            })
    }

    async fn unexpected(&self, response: Response, path: &str) -> DirectoryError {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        DirectoryError::Unexpected {
            path: path.to_string(),
            status,
            body,
        }
    }
}

/// The URL of the listing page after this one, from `Link: <uri>; rel="next"`.
fn next_page(response: &Response) -> Option<String> {
    let value = response.headers().get(LINK)?.to_str().ok()?;
    if !value.contains("rel=\"next\"") {
        return None;
    }
    let start = value.find('<')? + 1;
    let end = start + value[start..].find('>')?;
    Some(value[start..end].to_string())
}

fn read_etag(response: &Response) -> Option<String> {
    response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

#[cfg(test)]
#[path = "directory_test.rs"]
mod directory_test;
