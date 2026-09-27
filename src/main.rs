#![allow(clippy::needless_return)]

mod flags;

use std::{fs, time::Duration};

use anyhow::{Context, Result, bail};
use reqwest::{
    Method, Url,
    blocking::{Client, RequestBuilder},
};
use serde_json::{Map, Value, json};
use uuid::Uuid;

const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:18087";
const REQUIRED_ENGINES: [&str; 3] = ["playwright", "puppeteer", "selenium"];

#[derive(Clone)]
struct DaemonClient {
    base_url: Url,
    token: String,
    http: Client,
}

impl DaemonClient {
    fn from_env(env: &flags::EnvMap) -> Result<Self> {
        let raw = env
            .get("TKDA_LOCAL_CONTROL_URL")
            .map(String::as_str)
            .unwrap_or(DEFAULT_DAEMON_URL);
        let base_url = Url::parse(raw).context("invalid TKDA_LOCAL_CONTROL_URL")?;
        validate_loopback_url(&base_url)?;
        let token = read_secret(
            env,
            "TKDA_LOCAL_CONTROL_TOKEN",
            "TKDA_LOCAL_CONTROL_TOKEN_FILE",
            32,
            4096,
        )?;
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(130))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to construct daemon HTTP client")?;
        return Ok(Self {
            base_url,
            token,
            http,
        });
    }

    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let url = self
            .base_url
            .join(path)
            .context("failed to construct daemon URL")?;
        return Ok(self.http.request(method, url).bearer_auth(&self.token));
    }

    fn json(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut request = self.request(method, path)?;
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().context("desktop daemon request failed")?;
        let status = response.status();
        let value = response
            .json::<Value>()
            .unwrap_or_else(|_| json!({"error": "desktop daemon returned a non-JSON response"}));
        if !status.is_success() {
            bail!(
                "desktop daemon returned HTTP {}: {}",
                status.as_u16(),
                value
            );
        }
        return Ok(value);
    }
}

fn validate_loopback_url(url: &Url) -> Result<()> {
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("TKDA_LOCAL_CONTROL_URL must be credential-free loopback HTTP");
    }
    let host = url
        .host_str()
        .context("TKDA_LOCAL_CONTROL_URL is missing a host")?;
    if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
        bail!("TKDA_LOCAL_CONTROL_URL must target loopback");
    }
    return Ok(());
}

fn read_secret(
    env: &flags::EnvMap,
    inline_key: &str,
    file_key: &str,
    min_len: usize,
    max_len: usize,
) -> Result<String> {
    let raw = match env.get(inline_key) {
        Some(value) => value.clone(),
        None => {
            let path = env
                .get(file_key)
                .with_context(|| format!("set {inline_key} or {file_key}"))?;
            fs::read_to_string(path)
                .with_context(|| format!("failed to read {file_key} at {path}"))?
        }
    };
    let token = raw.trim();
    if token.len() < min_len || token.len() > max_len || token.chars().any(char::is_whitespace) {
        bail!("{inline_key} must contain {min_len}..={max_len} non-whitespace characters");
    }
    return Ok(token.to_owned());
}

fn required<'a>(env: &'a flags::EnvMap, key: &str) -> Result<&'a str> {
    return env
        .get(key)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("{key} is required for this command"));
}

fn bool_value(env: &flags::EnvMap, key: &str) -> bool {
    return env.get(key).map(String::as_str) == Some("true");
}

fn run_body(env: &flags::EnvMap) -> Result<Value> {
    let prompt = required(env, "TKDA_PROMPT")?.trim();
    if prompt.chars().count() > 50_000 {
        bail!("TKDA_PROMPT exceeds the 50000-character policy limit");
    }

    let language = env
        .get("TKDA_WORKER_LANGUAGE")
        .map(String::as_str)
        .unwrap_or("typescript");
    if !matches!(language, "typescript" | "python" | "rust" | "go") {
        bail!("unsupported TKDA_WORKER_LANGUAGE");
    }

    let engine = env
        .get("TKDA_BROWSER_ENGINE")
        .map(String::as_str)
        .unwrap_or("playwright");
    if !REQUIRED_ENGINES.contains(&engine) {
        bail!("unsupported TKDA_BROWSER_ENGINE");
    }

    let mut body = Map::new();
    body.insert(
        "task_id".into(),
        Value::String(format!("desktop-cli-{}", Uuid::new_v4())),
    );
    body.insert("prompt".into(), Value::String(prompt.to_owned()));
    body.insert("language".into(), Value::String(language.to_owned()));
    body.insert("browser_engine".into(), Value::String(engine.to_owned()));
    body.insert(
        "execution_mode".into(),
        Value::String(if bool_value(env, "TKDA_HEADED") {
            "headed".into()
        } else {
            "headless".into()
        }),
    );
    body.insert("execution_target".into(), Value::String("local".into()));
    body.insert(
        "placement_preference".into(),
        Value::String("desktop".into()),
    );
    body.insert("timeout_secs".into(), Value::from(1800));
    body.insert("max_retries".into(), Value::from(2));
    body.insert(
        "ai".into(),
        json!({"enabled": true, "max_planning_steps": 16, "max_replans": 3}),
    );
    return Ok(Value::Object(body));
}

fn doctor(status: Value) -> Result<Value> {
    if status.get("ok").and_then(Value::as_bool) != Some(true) {
        bail!("desktop daemon reported unhealthy status");
    }
    if status.get("protocol_version").and_then(Value::as_u64) != Some(1) {
        bail!("desktop daemon protocol_version is not 1");
    }
    if status.get("surface").and_then(Value::as_str) != Some("desktop_daemon") {
        bail!("unexpected desktop daemon control surface");
    }

    let engines = status
        .get("engines")
        .and_then(Value::as_array)
        .context("desktop daemon status omitted engines")?;
    let missing = REQUIRED_ENGINES
        .iter()
        .filter(|engine| !engines.iter().any(|value| value.as_str() == Some(**engine)))
        .copied()
        .collect::<Vec<_>>();

    return Ok(json!({
        "ok": missing.is_empty(),
        "protocol_version": 1,
        "surface": "desktop_daemon",
        "required_engines": REQUIRED_ENGINES,
        "missing_engines": missing,
        "daemon": status
    }));
}

fn app_command_body(env: &flags::EnvMap) -> Result<Value> {
    let app = env
        .get("TKDA_DESKTOP_APP")
        .map(String::as_str)
        .unwrap_or("any");
    if !matches!(app, "any" | "flutter" | "rust") {
        bail!("TKDA_DESKTOP_APP must be any, flutter, or rust");
    }

    let action = required(env, "TKDA_DESKTOP_APP_ACTION")?;
    if !matches!(action, "focus" | "open_run" | "open_task" | "open_settings") {
        bail!("unsupported TKDA_DESKTOP_APP_ACTION");
    }

    let target = env
        .get("TKDA_DESKTOP_APP_TARGET")
        .map(String::as_str)
        .filter(|value| !value.is_empty());
    match action {
        "open_run" | "open_task" if target.is_none() => {
            bail!("{action} requires TKDA_DESKTOP_APP_TARGET")
        }
        "focus" | "open_settings" if target.is_some() => {
            bail!("{action} does not accept TKDA_DESKTOP_APP_TARGET")
        }
        _ => {}
    }
    if target.is_some_and(|value| value.chars().count() > 256) {
        bail!("TKDA_DESKTOP_APP_TARGET exceeds 256 characters");
    }

    return Ok(json!({
        "app": app,
        "kind": action,
        "target": target
    }));
}

fn main() -> Result<()> {
    let env = flags::apply_cli_flags().map_err(anyhow::Error::msg)?;
    let command = env
        .get("TKDA_DESKTOP_COMMAND")
        .map(String::as_str)
        .unwrap_or("status");
    let client = DaemonClient::from_env(&env)?;

    let value = match command {
        "status" => client.json(Method::GET, "/v1/status", None)?,
        "doctor" => doctor(client.json(Method::GET, "/v1/status", None)?)?,
        "run" => client.json(Method::POST, "/v1/runs", Some(run_body(&env)?))?,
        "get" => {
            let run_id = required(&env, "TKDA_RUN_ID")?;
            Uuid::parse_str(run_id).context("TKDA_RUN_ID must be a UUID")?;
            client.json(Method::GET, &format!("/v1/runs/{run_id}"), None)?
        }
        "cancel" => {
            let run_id = required(&env, "TKDA_RUN_ID")?;
            Uuid::parse_str(run_id).context("TKDA_RUN_ID must be a UUID")?;
            client.json(
                Method::POST,
                &format!("/v1/runs/{run_id}/cancel"),
                Some(json!({})),
            )?
        }
        "keep-awake" => client.json(
            Method::POST,
            "/v1/power/keep-awake",
            Some(json!({"enabled": bool_value(&env, "TKDA_KEEP_AWAKE")})),
        )?,
        "apps" => client.json(Method::GET, "/v1/apps", None)?,
        "app-command" => client.json(
            Method::POST,
            "/v1/apps/command",
            Some(app_command_body(&env)?),
        )?,
        other => bail!("unsupported TKDA_DESKTOP_COMMAND={other}"),
    };

    println!("{}", serde_json::to_string_pretty(&value)?);
    return Ok(());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_loopback_daemon_urls() {
        assert!(validate_loopback_url(&Url::parse("https://example.com").unwrap()).is_err());
    }

    #[test]
    fn accepts_ipv4_loopback_daemon_url() {
        assert!(validate_loopback_url(&Url::parse(DEFAULT_DAEMON_URL).unwrap()).is_ok());
    }

    #[test]
    fn doctor_requires_all_browser_engines() {
        let ok = doctor(json!({
            "ok": true,
            "protocol_version": 1,
            "surface": "desktop_daemon",
            "engines": ["playwright", "puppeteer", "selenium"]
        }))
        .unwrap();
        assert_eq!(ok["ok"], true);

        let missing = doctor(json!({
            "ok": true,
            "protocol_version": 1,
            "surface": "desktop_daemon",
            "engines": ["playwright"]
        }))
        .unwrap();
        assert_eq!(missing["ok"], false);
    }
}
