//! Attach by-human's renewable ChatGPT credential at each model request.
use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    auth::AuthManager,
    engine::{
        AuthMode, EngineError, EngineErrorKind, Model, ModelTurn, OpenAIProvider, ProviderConfig,
        RuntimeSink,
    },
};

pub struct ChatGptModel {
    auth: AuthManager,
    provider: OpenAIProvider,
}

impl ChatGptModel {
    pub fn new(auth: AuthManager, model: String) -> crate::engine::Result<Self> {
        // This template is never dispatched directly. with_access_token clones
        // its shared HTTP client and supplies a current token for every turn.
        let mut config = ProviderConfig::new(model, "managed-at-request-time".into());
        config.auth_mode = AuthMode::ChatGptPlan;
        Ok(Self {
            auth,
            provider: OpenAIProvider::new(config)?,
        })
    }
}

#[async_trait]
impl Model for ChatGptModel {
    async fn turn(
        &self,
        input: &[Value],
        sink: &dyn RuntimeSink,
        cancel: &CancellationToken,
    ) -> crate::engine::Result<ModelTurn> {
        if cancel.is_cancelled() {
            return Err(EngineError::cancelled());
        }
        sink.checkpoint().await?;
        let token = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(EngineError::cancelled()),
            result = self.auth.access_token() => result.map_err(|_| EngineError::new(
                EngineErrorKind::Provider, "ChatGPT login must be renewed or is unavailable"
            ))?,
        };
        self.provider
            .with_access_token(token.as_str())?
            .turn(input, sink, cancel)
            .await
    }
}
