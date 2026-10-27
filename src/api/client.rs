use std::time::Duration;

use eventsource_stream::Eventsource;
use futures_util::{StreamExt, stream::BoxStream};
use reqwest::{Method, RequestBuilder, Response, StatusCode, Url};
use serde::de::DeserializeOwned;

use super::types::{
    BuildDetail, BuildPage, EffectEvent, EffectRun, LogEvent, QueueEntry, Repo, RepoRef,
    StatusEvent,
};
use crate::{actions::Action, config::Token};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP {status}: {detail}")]
    Status { status: StatusCode, detail: String },
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("invalid server URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("event stream: {0}")]
    Stream(String),
    #[error("malformed {event:?} event: {source}")]
    Event {
        event: String,
        source: serde_json::Error,
    },
    #[error("no API token configured for {0}")]
    NoToken(String),
}

impl Error {
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Status { status, .. } if *status == StatusCode::NOT_FOUND)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub type EventStream<T> = BoxStream<'static, Result<T>>;

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    token: Option<Token>,
}

impl Client {
    pub fn new(base: &str, token: Option<Token>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("nixbot-tui/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base: Url::parse(base)?,
            token,
        })
    }

    pub fn base(&self) -> &str {
        self.base.as_str().trim_end_matches('/')
    }

    fn url(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("http(s) URLs have a path")
            .pop_if_empty()
            .extend(segments);
        url
    }

    fn repo_url(&self, repo: &RepoRef, rest: &[&str]) -> Url {
        let mut segments = vec!["api", "repos", &repo.forge, &repo.owner, &repo.name];
        segments.extend_from_slice(rest);
        self.url(&segments)
    }

    fn request(&self, method: Method, url: Url) -> RequestBuilder {
        let request = self.http.request(method, url);
        match &self.token {
            Some(token) => request.bearer_auth(token.expose()),
            None => request,
        }
    }

    async fn send(request: RequestBuilder) -> Result<Response> {
        let response = request.send().await?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = response.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                v.get("detail")
                    .map(|d| d.as_str().map_or(d.to_string(), str::to_owned))
            })
            .unwrap_or(body);
        Err(Error::Status { status, detail })
    }

    async fn get_json<T: DeserializeOwned>(&self, url: Url) -> Result<T> {
        let request = self.request(Method::GET, url).timeout(REQUEST_TIMEOUT);
        Ok(Self::send(request).await?.json().await?)
    }

    async fn get_text(&self, url: Url) -> Result<String> {
        let request = self.request(Method::GET, url).timeout(REQUEST_TIMEOUT);
        Ok(Self::send(request).await?.text().await?)
    }

    pub async fn repos(&self) -> Result<Vec<Repo>> {
        self.get_json(self.url(&["api", "repos"])).await
    }

    pub async fn queue(&self) -> Result<Vec<QueueEntry>> {
        self.get_json(self.url(&["api", "queue"])).await
    }

    pub async fn builds(&self, repo: &RepoRef, page: u32) -> Result<BuildPage> {
        let mut url = self.repo_url(repo, &["builds"]);
        url.query_pairs_mut().append_pair("page", &page.to_string());
        self.get_json(url).await
    }

    pub async fn build(&self, repo: &RepoRef, number: u64) -> Result<BuildDetail> {
        self.get_json(self.repo_url(repo, &["builds", &number.to_string()]))
            .await
    }

    pub async fn log_text(
        &self,
        repo: &RepoRef,
        number: u64,
        attr: &str,
        tail: u32,
    ) -> Result<String> {
        let number = number.to_string();
        let mut url = self.repo_url(repo, &["builds", &number, "logs", attr, "text"]);
        url.query_pairs_mut()
            .append_pair("tail", &tail.to_string())
            .append_pair("ansi", "1");
        self.get_text(url).await
    }

    pub async fn effect_run(&self, repo: &RepoRef, run_id: u64) -> Result<EffectRun> {
        self.get_json(self.repo_url(repo, &["effects", "runs", &run_id.to_string()]))
            .await
    }

    pub async fn effect_run_text(&self, repo: &RepoRef, run_id: u64, tail: u32) -> Result<String> {
        let id = run_id.to_string();
        let mut url = self.repo_url(repo, &["effects", "runs", &id, "text"]);
        url.query_pairs_mut()
            .append_pair("tail", &tail.to_string())
            .append_pair("ansi", "1");
        self.get_text(url).await
    }

    async fn sse<T: Send + 'static>(
        &self,
        url: Url,
        parse: fn(&str, &str) -> serde_json::Result<Option<T>>,
    ) -> Result<EventStream<T>> {
        let response = Self::send(self.request(Method::GET, url)).await?;
        let events = response
            .bytes_stream()
            .eventsource()
            .filter_map(move |event| {
                std::future::ready(match event {
                    Ok(event) => parse(&event.event, &event.data)
                        .map_err(|source| Error::Event {
                            event: event.event,
                            source,
                        })
                        .transpose(),
                    Err(err) => Some(Err(Error::Stream(err.to_string()))),
                })
            });
        Ok(events.boxed())
    }

    /// Live output of a running attribute; history comes from [`Self::log_text`].
    pub async fn log_stream(
        &self,
        repo: &RepoRef,
        number: u64,
        attr: &str,
    ) -> Result<EventStream<LogEvent>> {
        let number = number.to_string();
        let url = self.repo_url(repo, &["builds", &number, "logs", attr, "stream"]);
        self.sse(url, LogEvent::parse).await
    }

    /// The last `tail` lines of an effect run's output, then its live output.
    pub async fn effect_stream(
        &self,
        repo: &RepoRef,
        run_id: u64,
        tail: u32,
    ) -> Result<EventStream<EffectEvent>> {
        let id = run_id.to_string();
        let mut url = self.repo_url(repo, &["effects", "runs", &id, "stream"]);
        url.query_pairs_mut().append_pair("tail", &tail.to_string());
        self.sse(url, EffectEvent::parse).await
    }

    /// Status-change cues for every build this token can see.
    pub async fn status_events(&self) -> Result<EventStream<StatusEvent>> {
        fn parse(event: &str, data: &str) -> serde_json::Result<Option<StatusEvent>> {
            (event == "status")
                .then(|| serde_json::from_str(data))
                .transpose()
        }
        self.sse(self.url(&["api", "events"]), parse).await
    }

    pub async fn control(&self, repo: &RepoRef, number: u64, action: &Action) -> Result<()> {
        if self.token.is_none() {
            return Err(Error::NoToken(self.base().to_owned()));
        }
        let number = number.to_string();
        let (segments, query) = action.endpoint();
        let mut path = vec!["builds", number.as_str()];
        path.extend(segments);
        let mut url = self.repo_url(repo, &path);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Self::send(self.request(Method::POST, url).timeout(REQUEST_TIMEOUT)).await?;
        Ok(())
    }

    /// The build's page in the web UI.
    pub fn build_page(&self, repo: &RepoRef, number: u64) -> String {
        let number = number.to_string();
        self.url(&[
            "repos",
            &repo.forge,
            &repo.owner,
            &repo.name,
            "builds",
            &number,
        ])
        .into()
    }

    pub fn log_page(&self, repo: &RepoRef, number: u64, attr: &str) -> String {
        let number = number.to_string();
        let segments = [
            "repos",
            &repo.forge,
            &repo.owner,
            &repo.name,
            "builds",
            &number,
        ];
        let mut url = self.url(&segments);
        url.path_segments_mut()
            .expect("http(s) URLs have a path")
            .extend(["logs", attr]);
        url.into()
    }
}
