//! HTTP client for the acceptrequest server's client API.

use acceptrequest_common::{Policy, TimeRequest, UsageReport};
use anyhow::{Context, Result};
use std::time::Duration;

pub struct ServerApi {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl ServerApi {
    pub fn new(base: String, token: Option<String>) -> Result<ServerApi> {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
        Ok(ServerApi { http, base, token })
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    pub async fn policy(&self, user: &str) -> Result<Policy> {
        let url = format!("{}/api/client/policy/{user}", self.base);
        let resp = self.auth(self.http.get(&url)).send().await?.error_for_status()?;
        resp.json().await.context("decoding policy")
    }

    pub async fn report_usage(&self, report: &UsageReport) -> Result<()> {
        let url = format!("{}/api/client/usage", self.base);
        self.auth(self.http.post(&url)).json(report).send().await?.error_for_status()?;
        Ok(())
    }

    pub async fn request_time(&self, request: &TimeRequest) -> Result<()> {
        let url = format!("{}/api/client/requests", self.base);
        self.auth(self.http.post(&url)).json(request).send().await?.error_for_status()?;
        Ok(())
    }
}
