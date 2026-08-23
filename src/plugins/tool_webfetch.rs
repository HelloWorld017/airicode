use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{header, redirect::Policy, Client, Url};
use serde::{Deserialize, Serialize};

use crate::core::{
    Error, Plugin, PluginId, PluginRegistrar, Result, Tool, ToolContext, ToolDefinition, ToolId,
    ToolOutput,
};

const PLUGIN_ID: &str = "builtin.tool-webfetch";
const WEBFETCH_TOOL_ID: &str = "external.webfetch";
const WEBFETCH_TOOL_NAME: &str = "webfetch";

const DEFAULT_MAX_BYTES: usize = 1024 * 1024;
const HARD_MAX_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_REDIRECTS: usize = 5;
const HARD_MAX_REDIRECTS: usize = 10;
const DEFAULT_TIMEOUT_SECS: u64 = 20;
const HARD_TIMEOUT_SECS: u64 = 120;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebFetchConfig {
    pub max_bytes: usize,
    pub max_redirects: usize,
    pub timeout_secs: u64,
    pub content_types: Vec<String>,
}

impl Default for WebFetchConfig {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            max_redirects: DEFAULT_MAX_REDIRECTS,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            content_types: default_content_types(),
        }
    }
}

struct WebFetchPlugin {
    config: WebFetchConfig,
}

/// A bounded HTTP fetch tool with literal-IP SSRF checks.
///
/// This does not pin or inspect DNS answers. A hostname that resolves to a public address and is
/// later rebound to a private address cannot be prevented with reqwest's public API alone. Deploy
/// this tool behind an egress proxy or network sandbox when hostile DNS names are in scope.
#[derive(Clone)]
struct WebFetchTool {
    client: Client,
    config: WebFetchConfig,
}

impl WebFetchTool {
    fn new(config: WebFetchConfig) -> Result<Self> {
        let client = Client::builder()
            .redirect(Policy::none())
            .build()
            .map_err(|error| Error::Tool(format!("could not build webfetch client: {error}")))?;
        Ok(Self { client, config })
    }
}

pub fn webfetch_plugin(config: WebFetchConfig) -> Arc<dyn Plugin> {
    Arc::new(WebFetchPlugin { config })
}

#[async_trait]
impl Plugin for WebFetchPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        validate_config(&self.config)?;
        registrar.register_tool(0, Arc::new(WebFetchTool::new(self.config.clone())?))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WebFetchInput {
    url: String,
    #[serde(default)]
    max_bytes: Option<usize>,
    #[serde(default)]
    max_redirects: Option<usize>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    content_types: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct WebFetchResponse {
    url: String,
    status: u16,
    content_type: String,
    bytes: usize,
    truncated: bool,
    redirects: usize,
    dns_rebinding_protected: bool,
    text: String,
}

fn default_content_types() -> Vec<String> {
    vec![
        "text/".into(),
        "application/json".into(),
        "application/xml".into(),
        "application/xhtml+xml".into(),
    ]
}

fn validate_config(config: &WebFetchConfig) -> Result<()> {
    if config.max_bytes == 0 || config.max_bytes > HARD_MAX_BYTES {
        return Err(Error::Plugin(format!(
            "webfetch max_bytes must be between 1 and {HARD_MAX_BYTES}"
        )));
    }
    if config.max_redirects > HARD_MAX_REDIRECTS {
        return Err(Error::Plugin(format!(
            "webfetch max_redirects may not exceed {HARD_MAX_REDIRECTS}"
        )));
    }
    if config.timeout_secs == 0 || config.timeout_secs > HARD_TIMEOUT_SECS {
        return Err(Error::Plugin(format!(
            "webfetch timeout_secs must be between 1 and {HARD_TIMEOUT_SECS}"
        )));
    }
    if config.content_types.is_empty()
        || config
            .content_types
            .iter()
            .any(|content_type| content_type.trim().is_empty())
    {
        return Err(Error::Plugin(
            "webfetch content_types may not be empty".into(),
        ));
    }
    Ok(())
}

fn validate_url(value: &str) -> Result<Url> {
    let url =
        Url::parse(value).map_err(|error| Error::Tool(format!("invalid webfetch URL: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::Tool("webfetch URL must use http or https".into()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::Tool(
            "webfetch URL may not contain credentials".into(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| Error::Tool("webfetch URL must contain a host".into()))?;
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err(Error::Tool("webfetch URL may not target localhost".into()));
    }
    let ip_literal = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = ip_literal.parse::<IpAddr>() {
        if is_disallowed_ip(ip) {
            return Err(Error::Tool(format!(
                "webfetch URL targets a non-public IP address: {ip}"
            )));
        }
    }
    Ok(url)
}

fn is_disallowed_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_disallowed_v4(ip),
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_disallowed_v4(mapped);
            }
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || is_v6_prefix(ip, Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7)
                || is_v6_prefix(ip, Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10)
        }
    }
}

fn is_disallowed_v4(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || a == 0
        || a >= 240
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
}

fn is_v6_prefix(ip: Ipv6Addr, prefix: Ipv6Addr, bits: u32) -> bool {
    let shift = 128 - bits;
    (u128::from(ip) >> shift) == (u128::from(prefix) >> shift)
}

fn content_type_allowed(actual: &str, allowed: &[String]) -> bool {
    let media_type = actual.split(';').next().unwrap_or("").trim();
    allowed.iter().any(|item| {
        let item = item.trim();
        if let Some(prefix) = item.strip_suffix('*') {
            !prefix.is_empty() && media_type.starts_with(prefix)
        } else if item.ends_with('/') {
            media_type.starts_with(item)
        } else {
            media_type == item
        }
    })
}

#[async_trait]
impl Tool for WebFetchTool {
    fn id(&self) -> ToolId {
        ToolId::new(WEBFETCH_TOOL_ID)
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: WEBFETCH_TOOL_NAME.into(),
            description: "Fetch bounded text from an HTTP(S) URL. Literal private addresses are blocked; DNS rebinding requires external egress controls.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["url"],
                "properties": {
                    "url": { "type": "string", "minLength": 1 },
                    "max_bytes": { "type": "integer", "minimum": 1, "maximum": self.config.max_bytes, "default": self.config.max_bytes },
                    "max_redirects": { "type": "integer", "minimum": 0, "maximum": self.config.max_redirects, "default": self.config.max_redirects },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": self.config.timeout_secs, "default": self.config.timeout_secs },
                    "content_types": { "type": "array", "default": self.config.content_types.clone(), "items": { "type": "string", "minLength": 1 } }
                }
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let input: WebFetchInput = serde_json::from_value(input)
            .map_err(|error| Error::Tool(format!("invalid webfetch input: {error}")))?;
        let max_bytes = input.max_bytes.unwrap_or(self.config.max_bytes);
        let max_redirects = input.max_redirects.unwrap_or(self.config.max_redirects);
        let timeout_secs = input.timeout_secs.unwrap_or(self.config.timeout_secs);
        let content_types = input
            .content_types
            .unwrap_or_else(|| self.config.content_types.clone());
        if max_bytes == 0 || max_bytes > self.config.max_bytes {
            return Err(Error::Tool(format!(
                "max_bytes must be between 1 and {}",
                self.config.max_bytes
            )));
        }
        if max_redirects > self.config.max_redirects {
            return Err(Error::Tool(format!(
                "max_redirects may not exceed {}",
                self.config.max_redirects
            )));
        }
        if timeout_secs == 0 || timeout_secs > self.config.timeout_secs {
            return Err(Error::Tool(format!(
                "timeout_secs must be between 1 and {}",
                self.config.timeout_secs
            )));
        }
        if content_types.is_empty() || content_types.iter().any(|value| value.trim().is_empty()) {
            return Err(Error::Tool("content_types may not be empty".into()));
        }

        let cancellation = context.cancellation;
        let operation = async {
            let mut url = validate_url(&input.url)?;
            let mut redirects = 0;
            let response = loop {
                let response =
                    self.client.get(url.clone()).send().await.map_err(|error| {
                        Error::Tool(format!("webfetch request failed: {error}"))
                    })?;
                if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                    break response;
                }
                if redirects == max_redirects {
                    return Err(Error::Tool("webfetch redirect limit exceeded".into()));
                }
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .ok_or_else(|| Error::Tool("redirect response has no Location header".into()))?
                    .to_str()
                    .map_err(|_| Error::Tool("redirect Location is not valid text".into()))?;
                let next = url
                    .join(location)
                    .map_err(|error| Error::Tool(format!("invalid redirect Location: {error}")))?;
                url = validate_url(next.as_str())?;
                redirects += 1;
            };

            let status = response.status();
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned();
            if !content_type_allowed(&content_type, &content_types) {
                return Err(Error::Tool(format!(
                    "webfetch content type is not allowed: {content_type}"
                )));
            }
            if let Some(length) = response.content_length() {
                if length > max_bytes as u64 {
                    return Err(Error::Tool(format!(
                        "webfetch response exceeds {} bytes",
                        max_bytes
                    )));
                }
            }
            let mut stream = response.bytes_stream();
            let mut bytes = Vec::new();
            let mut truncated = false;
            while let Some(chunk) = stream.next().await {
                let chunk =
                    chunk.map_err(|error| Error::Tool(format!("webfetch body failed: {error}")))?;
                let remaining = max_bytes.saturating_sub(bytes.len());
                if chunk.len() > remaining {
                    bytes.extend_from_slice(&chunk[..remaining]);
                    truncated = true;
                    break;
                }
                bytes.extend_from_slice(&chunk);
            }
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let content = serde_json::to_string(&WebFetchResponse {
                url: url.to_string(),
                status: status.as_u16(),
                content_type,
                bytes: bytes.len(),
                truncated,
                redirects,
                dns_rebinding_protected: false,
                text,
            })
            .map_err(|error| Error::Tool(format!("could not encode webfetch output: {error}")))?;
            Ok(ToolOutput {
                content,
                is_error: !status.is_success(),
            })
        };

        tokio::select! {
            _ = cancellation.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(Duration::from_secs(timeout_secs), operation) =>
                result.map_err(|_| Error::Tool("webfetch request timed out".into()))?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Core;

    #[test]
    fn validates_schemes_and_literal_addresses() {
        assert!(validate_url("https://example.com/a").is_ok());
        assert!(validate_url("ftp://example.com/a").is_err());
        assert!(validate_url("http://127.0.0.1/a").is_err());
        assert!(validate_url("http://10.1.2.3/a").is_err());
        assert!(validate_url("http://169.254.1.2/a").is_err());
        assert!(validate_url("http://[::1]/a").is_err());
        assert!(validate_url("http://[fc00::1]/a").is_err());
        assert!(validate_url("http://[::ffff:127.0.0.1]/a").is_err());
    }

    #[test]
    fn content_type_matching_ignores_parameters() {
        assert!(content_type_allowed(
            "text/html; charset=utf-8",
            &["text/".into()]
        ));
        assert!(!content_type_allowed("image/png", &["text/".into()]));
        assert!(!content_type_allowed(
            "application/json-seq",
            &["application/json".into()]
        ));
    }

    #[tokio::test]
    async fn plugin_registration_and_omission_control_tool_registry() {
        let included = Core::new()
            .with_plugin(webfetch_plugin(WebFetchConfig::default()))
            .build()
            .await
            .unwrap();
        assert!(included
            .tools()
            .get(&ToolId::new(WEBFETCH_TOOL_ID))
            .is_some());

        let omitted = Core::new().build().await.unwrap();
        assert!(omitted
            .tools()
            .get(&ToolId::new(WEBFETCH_TOOL_ID))
            .is_none());
    }
}
