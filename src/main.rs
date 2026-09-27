use std::{collections::HashMap, fs, io::Write, path::Path};

use anyhow::{Context, Result, bail};
use flags2env::BundledFlags2Env;
use reqwest::{Client, Method, Url, header};
use serde_json::{Value, json};
use tempfile::NamedTempFile;
use uuid::Uuid;

const FLAGS_CONTRACT: &str = include_str!("../.cli-flags.toml");
const MAX_JSON_BYTES: u64 = 1_048_576;

type EnvMap = HashMap<String, String>;

#[derive(Debug)]
struct Config {
    command: String,
    daemon_url: String,
    control_token: String,
    run_file: Option<String>,
    run_id: Option<String>,
    enabled: bool,
}

impl Config {
    fn from_env(env: &EnvMap) -> Result<Self> {
        let command = env
            .get("TKDA_DESKTOP_COMMAND")
            .cloned()
            .unwrap_or_else(|| "status".to_string());
        validate_command(&command)?;

        let daemon_url = env
            .get("TKDA_LOCAL_CONTROL_URL")
            .cloned()
            .unwrap_or_else(|| "http://127.0.0.1:18087".to_string());
        validate_loopback_url(&daemon_url)?;

        let enabled = env
            .get("TKDA_KEEP_AWAKE_ENABLED")
            .map(|value| value == "true")
            .unwrap_or(false);

        return Ok(Self {
            command,
            daemon_url,
            control_token: read_control_token(env)?,
            run_file: env.get("TKDA_RUN_FILE").cloned(),
            run_id: env.get("TKDA_RUN_ID").cloned(),
            enabled,
        });
    }
}

struct DesktopClient {
    client: Client,
    daemon_url: String,
    control_token: String,
}

impl DesktopClient {
    fn new(config: &Config) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(3))
            .timeout(std::time::Duration::from_secs(130))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to construct Takoda desktop client")?;

        return Ok(Self {
            client,
            daemon_url: config.daemon_url.trim_end_matches('/').to_string(),
            control_token: config.control_token.clone(),
        });
    }

    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        if !path.starts_with('/') || path.contains("..") || path.contains("://") {
            bail!("refusing invalid local-control path");
        }

        let mut request = self
            .client
            .request(method.clone(), format!("{}{}", self.daemon_url, path))
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.control_token),
            );

        if method != Method::GET {
            request = request.json(&body.unwrap_or_else(|| json!({})));
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("Takoda desktop daemon request failed for {path}"))?;
        let status = response.status();
        if response.content_length().unwrap_or(0) > MAX_JSON_BYTES {
            bail!("Takoda desktop daemon response exceeds 1 MiB policy limit");
        }

        let bytes = response
            .bytes()
            .await
            .context("failed to read Takoda desktop daemon response")?;
        if bytes.len() as u64 > MAX_JSON_BYTES {
            bail!("Takoda desktop daemon response exceeds 1 MiB policy limit");
        }

        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice::<Value>(&bytes)
                .context("Takoda desktop daemon returned non-JSON content")?
        };

        if !status.is_success() {
            bail!("Takoda desktop daemon returned HTTP {}: {}", status.as_u16(), value);
        }

        return Ok(value);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let env = apply_cli_flags().map_err(anyhow::Error::msg)?;
    let config = Config::from_env(&env)?;
    let client = DesktopClient::new(&config)?;

    let result = match config.command.as_str() {
        "status" => client.request(Method::GET, "/v1/status", None).await?,
        "run" => {
            let run_file = config
                .run_file
                .as_deref()
                .context("--run-file is required for --command=run")?;
            let run = read_local_run(run_file)?;
            client.request(Method::POST, "/v1/runs", Some(run)).await?
        }
        "run-status" => {
            let run_id = require_run_id(config.run_id.as_deref())?;
            client
                .request(Method::GET, &format!("/v1/runs/{run_id}"), None)
                .await?
        }
        "run-cancel" => {
            let run_id = require_run_id(config.run_id.as_deref())?;
            client
                .request(
                    Method::POST,
                    &format!("/v1/runs/{run_id}/cancel"),
                    Some(json!({})),
                )
                .await?
        }
        "keep-awake" => {
            client
                .request(
                    Method::POST,
                    "/v1/power/keep-awake",
                    Some(json!({"enabled": config.enabled})),
                )
                .await?
        }
        _ => {
            bail!("unsupported command");
        }
    };

    println!("{}", serde_json::to_string_pretty(&result)?);
    return Ok(());
}

fn apply_cli_flags() -> std::result::Result<EnvMap, String> {
    let argv: Vec<String> = std::env::args().collect();
    let initial: EnvMap = std::env::vars().collect();

    let mut contract = NamedTempFile::new()
        .map_err(|error| format!("cannot create embedded flags-2-env contract: {error}"))?;
    contract
        .write_all(FLAGS_CONTRACT.as_bytes())
        .map_err(|error| format!("cannot materialize flags-2-env contract: {error}"))?;
    let path = contract
        .path()
        .to_str()
        .ok_or_else(|| "flags-2-env contract path is not valid UTF-8".to_owned())?;

    let parser = BundledFlags2Env::new();
    parser
        .audit_config(Some(path))
        .map_err(|error| format!("flags-2-env configuration audit failed: {error}"))?;
    let parsed = parser
        .parse_structured(&argv, Some(path))
        .map_err(|error| format!("flags-2-env parse failed: {error}"))?;

    if !parsed.unknown_options.is_empty() {
        let names = parsed
            .unknown_options
            .iter()
            .map(|option| option.split('=').next().unwrap_or_default())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!("unknown command-line option(s): {names}"));
    }
    if !parsed.errors.is_empty() {
        return Err(format!(
            "invalid command-line value(s): {}",
            parsed.errors.join("; ")
        ));
    }
    if !parsed.extras.is_empty() {
        return Err(format!(
            "unexpected positional argument(s): {}",
            parsed.extras.len()
        ));
    }

    let mut env = initial;
    env.extend(parsed.flags);
    return Ok(env);
}

fn validate_command(command: &str) -> Result<()> {
    if !matches!(
        command,
        "status" | "run" | "run-status" | "run-cancel" | "keep-awake"
    ) {
        bail!(
            "--command must be one of status, run, run-status, run-cancel, or keep-awake"
        );
    }

    return Ok(());
}

fn validate_loopback_url(raw: &str) -> Result<()> {
    let url = Url::parse(raw).context("--daemon-url must be a valid URL")?;
    if url.scheme() != "http" {
        bail!("--daemon-url must use http:// because the daemon is loopback-only");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("--daemon-url may not contain credentials");
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        bail!("--daemon-url must be an origin without a path, query, or fragment");
    }

    let host = url
        .host_str()
        .context("--daemon-url must include a host")?
        .to_ascii_lowercase();
    if !matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1") {
        bail!("--daemon-url must target loopback");
    }

    return Ok(());
}

fn read_control_token(env: &EnvMap) -> Result<String> {
    let raw = match env.get("TKDA_LOCAL_CONTROL_TOKEN") {
        Some(token) => token.clone(),
        None => {
            let path = env
                .get("TKDA_LOCAL_CONTROL_TOKEN_FILE")
                .context("set TKDA_LOCAL_CONTROL_TOKEN or TKDA_LOCAL_CONTROL_TOKEN_FILE")?;
            fs::read_to_string(path)
                .with_context(|| format!("failed to read local-control token file at {path}"))?
        }
    };

    let token = raw.trim();
    if token.len() < 32 || token.len() > 4096 || token.chars().any(char::is_whitespace) {
        bail!("Takoda local-control token must be 32..=4096 non-whitespace characters");
    }

    return Ok(token.to_string());
}

fn read_local_run(path: &str) -> Result<Value> {
    let path = Path::new(path);
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to stat run request {}", path.display()))?;
    if metadata.len() > MAX_JSON_BYTES {
        bail!("run request exceeds 1 MiB policy limit");
    }

    let bytes = fs::read(path)
        .with_context(|| format!("failed to read run request {}", path.display()))?;
    if bytes.len() as u64 > MAX_JSON_BYTES {
        bail!("run request exceeds 1 MiB policy limit");
    }

    let mut value: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid JSON in {}", path.display()))?;
    let object = value
        .as_object_mut()
        .context("run request must be a JSON object")?;
    object.insert("execution_target".to_string(), Value::String("local".to_string()));
    object.insert(
        "placement_preference".to_string(),
        Value::String("desktop".to_string()),
    );

    return Ok(value);
}

fn require_run_id(value: Option<&str>) -> Result<String> {
    let value = value.context("--run-id is required for this command")?;
    let parsed = Uuid::parse_str(value).context("--run-id must be a UUID")?;
    return Ok(parsed.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_url_must_be_loopback_origin() {
        assert!(validate_loopback_url("http://127.0.0.1:18087").is_ok());
        assert!(validate_loopback_url("http://localhost:18087").is_ok());
        assert!(validate_loopback_url("https://127.0.0.1:18087").is_err());
        assert!(validate_loopback_url("http://example.com:18087").is_err());
        assert!(validate_loopback_url("http://127.0.0.1:18087/v1").is_err());
    }

    #[test]
    fn command_surface_is_bounded() {
        assert!(validate_command("status").is_ok());
        assert!(validate_command("run").is_ok());
        assert!(validate_command("shell").is_err());
    }

    #[test]
    fn run_id_must_be_uuid() {
        assert!(require_run_id(Some("not-a-uuid")).is_err());
        assert!(require_run_id(None).is_err());
    }
}
