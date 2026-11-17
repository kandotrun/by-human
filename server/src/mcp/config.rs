//! Owner-selected MCP connections. Values contain references to credentials,
//! never automatically inherited application credentials.
use std::{collections::HashSet, fs::File, io::Read, net::IpAddr, path::PathBuf};

use serde::Deserialize;

use super::{McpError, Result};

const MAX_CONFIG_BYTES: usize = 1024 * 1024;

/// Deliberately not Debug: endpoint paths and command arguments may be private.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    pub servers: Vec<McpServerConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    pub id: String,
    pub transport: McpTransportConfig,
    pub tools: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpTransportConfig {
    Stdio {
        command: PathBuf,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: Vec<String>,
        #[serde(default)]
        cwd: Option<PathBuf>,
    },
    Http {
        url: String,
        #[serde(default)]
        bearer_token_env: Option<String>,
    },
}

fn default_timeout() -> u64 {
    30
}

impl McpConfig {
    pub fn from_env() -> Result<Self> {
        let Some(path) = std::env::var_os("BY_HUMAN_MCP_CONFIG") else {
            return Ok(Self::default());
        };
        if path.is_empty() {
            return Err(config_error());
        }
        let file = File::open(path).map_err(|_| config_error())?;
        if !file.metadata().map_err(|_| config_error())?.is_file() {
            return Err(config_error());
        }
        let mut bytes = Vec::new();
        file.take((MAX_CONFIG_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| config_error())?;
        Self::from_json(&bytes)
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(config_error());
        }
        let config: Self = serde_json::from_slice(bytes).map_err(|_| config_error())?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        let mut ids = HashSet::new();
        let total_tools: usize = self.servers.iter().map(|server| server.tools.len()).sum();
        if self.servers.len() > 8 || total_tools > 127 {
            return Err(config_error());
        }
        for server in &self.servers {
            if server.id.is_empty()
                || server.id.len() > 32
                || !server
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_')
                || !ids.insert(&server.id)
                || !(1..=120).contains(&server.timeout_seconds)
                || server.tools.is_empty()
                || server.tools.len() > 32
            {
                return Err(config_error());
            }
            let mut names = HashSet::new();
            for tool in &server.tools {
                if tool.is_empty()
                    || tool.len() > 128
                    || tool.chars().any(char::is_control)
                    || !names.insert(tool)
                {
                    return Err(config_error());
                }
            }
            match &server.transport {
                McpTransportConfig::Stdio {
                    command,
                    args,
                    env,
                    cwd,
                } => {
                    if !command.is_absolute()
                        || command.as_os_str().as_encoded_bytes().contains(&0)
                        || cwd.as_ref().is_some_and(|path| !path.is_absolute())
                        || args.len() > 64
                        || args
                            .iter()
                            .any(|arg| arg.len() > 4096 || arg.contains('\0'))
                        || env.len() > 32
                        || env.iter().any(|name| !valid_env_name(name))
                        || env.iter().collect::<HashSet<_>>().len() != env.len()
                    {
                        return Err(config_error());
                    }
                }
                McpTransportConfig::Http {
                    url,
                    bearer_token_env,
                } => {
                    let url = reqwest::Url::parse(url).map_err(|_| config_error())?;
                    let loopback = url
                        .host_str()
                        .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
                        .is_some_and(|ip| ip.is_loopback());
                    if !matches!(url.scheme(), "http" | "https")
                        || (url.scheme() == "http" && !loopback)
                        || url.host_str().is_none()
                        || !url.username().is_empty()
                        || url.password().is_some()
                        || url.fragment().is_some()
                        || bearer_token_env
                            .as_ref()
                            .is_some_and(|name| !valid_env_name(name))
                    {
                        return Err(config_error());
                    }
                }
            }
        }
        Ok(())
    }
}

fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn config_error() -> McpError {
    McpError::new("MCP configuration is invalid or unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn config() -> Value {
        json!({"servers":[{"id":"local", "tools":["read_note"],
            "transport":{"type":"stdio", "command":"/usr/bin/example", "env":["MCP_SECRET"]}}]})
    }

    fn accepts(value: Value) -> bool {
        McpConfig::from_json(&serde_json::to_vec(&value).unwrap()).is_ok()
    }

    #[test]
    fn explicit_connections_require_tools_and_unique_names() {
        assert!(accepts(config()));
        assert!(accepts(json!({"servers":[]})));
        for patch in [json!([]), json!(["a", "a"]), json!([""])] {
            let mut value = config();
            value["servers"][0]["tools"] = patch;
            assert!(!accepts(value));
        }
        let mut value = config();
        let duplicate = value["servers"][0].clone();
        value["servers"].as_array_mut().unwrap().push(duplicate);
        assert!(!accepts(value));
    }

    #[test]
    fn transport_cannot_accidentally_inherit_or_expose_credentials() {
        let mut value = config();
        value["servers"][0]["transport"]["command"] = json!("sh");
        assert!(!accepts(value));
        let mut value = config();
        value["servers"][0]["transport"]["env"] = json!({"SECRET":"private-fixture-value"});
        assert!(!accepts(value));
        let mut value = config();
        value["servers"][0]["transport"]["unknown"] = json!(true);
        assert!(!accepts(value));
        assert!(!accepts(json!({"servers":[], "discover_all":true})));
    }

    #[test]
    fn http_allows_https_and_literal_loopback_only() {
        for (url, expected) in [
            ("https://tools.example/mcp", true),
            ("http://127.0.0.1:1234/mcp", true),
            ("http://[::1]:1234/mcp", true),
            ("http://localhost/mcp", false),
            ("http://tools.example/mcp", false),
            ("https://user:password@tools.example/mcp", false),
            ("https://tools.example/mcp#fragment", false),
        ] {
            let mut value = config();
            value["servers"][0]["transport"] =
                json!({"type":"http", "url":url, "bearer_token_env":"MCP_SECRET"});
            assert_eq!(accepts(value), expected);
        }
    }

    #[test]
    fn invalid_config_errors_never_echo_private_values() {
        let error = McpConfig::from_json(br#"{"private-fixture-value": 1}"#)
            .err()
            .unwrap();
        assert!(!error.to_string().contains("private-fixture-value"));
        assert!(McpConfig::from_json(&vec![b' '; MAX_CONFIG_BYTES + 1]).is_err());
        let mut value = config();
        value["servers"][0]["timeout_seconds"] = json!(0);
        assert!(!accepts(value));
    }
}
