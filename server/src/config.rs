use std::{collections::HashMap, net::SocketAddr, path::PathBuf};

use crate::engine::AuthMode;

/// Secrets deliberately have no Debug implementation.
pub struct Config {
    pub database_url: String,
    pub listen: SocketAddr,
    pub api_token: String,
    pub openai_auth: AuthMode,
    pub openai_api_key: Option<String>,
    pub model: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let values = [
            "DATABASE_URL",
            "BY_HUMAN_LISTEN",
            "BY_HUMAN_API_TOKEN",
            "OPENAI_API_KEY",
            "OPENAI_MODEL",
            "BY_HUMAN_OPENAI_AUTH",
        ]
        .into_iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| (name.to_owned(), value))
        })
        .collect();
        Self::from_values(&values)
    }

    fn from_values(values: &HashMap<String, String>) -> anyhow::Result<Self> {
        let required = |name: &str| -> anyhow::Result<String> {
            let value = values.get(name).filter(|value| !value.trim().is_empty());
            value
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("{name} is required"))
        };
        let database_url = required("DATABASE_URL")?;
        let api_token = required("BY_HUMAN_API_TOKEN")?;
        anyhow::ensure!(
            (32..=512).contains(&api_token.len())
                && !api_token.starts_with("replace-with-")
                && api_token.bytes().all(|byte| byte.is_ascii_graphic()),
            "BY_HUMAN_API_TOKEN must contain 32 to 512 non-whitespace ASCII bytes"
        );
        let openai_auth = match values.get("BY_HUMAN_OPENAI_AUTH").map(String::as_str) {
            None | Some("chatgpt") => AuthMode::ChatGptPlan,
            Some("api-key") => AuthMode::ApiKey,
            Some(_) => anyhow::bail!("BY_HUMAN_OPENAI_AUTH must be chatgpt or api-key"),
        };
        let openai_api_key = if openai_auth == AuthMode::ApiKey {
            let key = required("OPENAI_API_KEY")?;
            anyhow::ensure!(
                api_token != key,
                "server authentication and OpenAI credentials must be different"
            );
            Some(key)
        } else {
            None
        };
        let listen = values
            .get("BY_HUMAN_LISTEN")
            .map(String::as_str)
            .unwrap_or("127.0.0.1:4310")
            .parse()
            .map_err(|_| anyhow::anyhow!("BY_HUMAN_LISTEN must be an IP address and port"))?;
        let model = values
            .get("OPENAI_MODEL")
            .cloned()
            .unwrap_or_else(|| "gpt-6.1-sol".to_owned());
        anyhow::ensure!(
            !model.trim().is_empty() && model.len() <= 128,
            "OPENAI_MODEL must be a model identifier"
        );
        Ok(Self {
            database_url,
            listen,
            api_token,
            openai_auth,
            openai_api_key,
            model,
        })
    }

    pub fn auth_directory() -> anyhow::Result<PathBuf> {
        if let Some(path) = std::env::var_os("BY_HUMAN_AUTH_DIR") {
            anyhow::ensure!(!path.is_empty(), "BY_HUMAN_AUTH_DIR must not be empty");
            return Ok(path.into());
        }
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("HOME or BY_HUMAN_AUTH_DIR is required"))?;
        Ok(PathBuf::from(home).join(".config/by-human"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> HashMap<String, String> {
        HashMap::from([
            ("DATABASE_URL".into(), "postgres://local-test-only".into()),
            ("BY_HUMAN_API_TOKEN".into(), "a".repeat(32)),
            (
                "OPENAI_API_KEY".into(),
                "synthetic-provider-credential".into(),
            ),
        ])
    }

    #[test]
    fn defaults_to_loopback_and_never_uses_the_provider_credential_for_clients() {
        let mut input = values();
        let config = Config::from_values(&input).unwrap();
        assert!(config.listen.ip().is_loopback());
        assert_eq!(config.openai_auth, AuthMode::ChatGptPlan);
        assert!(config.openai_api_key.is_none());
        input.insert("BY_HUMAN_OPENAI_AUTH".into(), "api-key".into());
        input.insert("BY_HUMAN_API_TOKEN".into(), "x".repeat(32));
        input.insert("OPENAI_API_KEY".into(), "x".repeat(32));
        assert!(Config::from_values(&input).is_err());
    }

    #[test]
    fn rejects_the_example_token_and_does_not_echo_secrets_in_errors() {
        let mut input = values();
        input.insert(
            "BY_HUMAN_API_TOKEN".into(),
            "replace-with-a-real-server-token-12345".into(),
        );
        let error = Config::from_values(&input).err().unwrap().to_string();
        assert!(!error.contains("12345"));
        input.insert("BY_HUMAN_LISTEN".into(), "private-credential-value".into());
        input.insert("BY_HUMAN_API_TOKEN".into(), "a".repeat(32));
        let error = Config::from_values(&input).err().unwrap().to_string();
        assert!(!error.contains("private-credential-value"));
    }

    #[test]
    fn rejects_non_header_tokens_before_starting_the_runner() {
        let mut input = values();
        for invalid in ["あ".repeat(32), "x".repeat(513), "x ".repeat(16)] {
            input.insert("BY_HUMAN_API_TOKEN".into(), invalid);
            assert!(Config::from_values(&input).is_err());
        }
    }

    #[test]
    fn chatgpt_login_never_requires_or_silently_uses_an_api_key() {
        let mut input = values();
        input.remove("OPENAI_API_KEY");
        assert!(Config::from_values(&input).is_ok());
        input.insert("BY_HUMAN_OPENAI_AUTH".into(), "api-key".into());
        assert!(Config::from_values(&input).is_err());
        input.insert("BY_HUMAN_OPENAI_AUTH".into(), "unknown".into());
        assert!(Config::from_values(&input).is_err());
    }
}
