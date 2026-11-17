use std::{sync::Arc, time::Duration};

use by_human::{
    api::{self, ApiState},
    auth::AuthManager,
    authenticated_model::ChatGptModel,
    config::Config,
    engine::{AuthMode, Engine, Limits, Model, OpenAIProvider, ProviderConfig, ToolRegistry},
    mcp::{McpConnections, config::McpConfig},
    runner::Runner,
    store::Store,
};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("login")
            if args.len() == 1
                || args.as_slice() == ["login", "--new-registration"]
                || args.as_slice() == ["login", "--enable-plan"] =>
        {
            outcome(login(args.get(1).map(String::as_str)).await)
        }
        Some("auth-status") if args.len() == 1 => outcome(auth_status().await),
        Some("mcp") if args.as_slice() == ["mcp", "list"] => outcome(mcp_list().await),
        Some("serve" | "check") if args.len() != 1 => {
            eprintln!("by-human: unexpected arguments; use --help");
            std::process::ExitCode::FAILURE
        }
        Some("serve") => match serve().await {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("by-human: {error}");
                std::process::ExitCode::FAILURE
            }
        },
        Some("check") => {
            let Ok(database_url) = std::env::var("DATABASE_URL") else {
                eprintln!("by-human: DATABASE_URL is required");
                return std::process::ExitCode::FAILURE;
            };
            match Store::connect(&database_url).await {
                Ok(_) => {
                    println!("by-human: PostgreSQL schema is ready");
                    std::process::ExitCode::SUCCESS
                }
                Err(_) => {
                    eprintln!("by-human: PostgreSQL could not be initialized");
                    std::process::ExitCode::FAILURE
                }
            }
        }
        None | Some("--help" | "-h") => {
            println!(
                "Usage: by-human <login|auth-status|mcp list|check|serve>\n\nlogin [--enable-plan|--new-registration]  Sign in or explicitly request plan consent.\nauth-status  Show safe OpenAI login readiness.\nmcp list     Connect configured MCP servers and list the enabled tools.\ncheck        Apply PostgreSQL migrations and check readiness.\nserve        Start the authenticated HTTP API and single-owner runner."
            );
            std::process::ExitCode::SUCCESS
        }
        Some(_) => {
            eprintln!("by-human: unknown command; use --help");
            std::process::ExitCode::FAILURE
        }
    }
}

fn outcome(result: anyhow::Result<()>) -> std::process::ExitCode {
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("by-human: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn login(option: Option<&str>) -> anyhow::Result<()> {
    let auth = AuthManager::new(Config::auth_directory()?)?;
    let pending = if option == Some("--enable-plan") {
        auth.begin_plan_consent().await?
    } else {
        auth.begin_login(option == Some("--new-registration"))
            .await?
    };
    println!(
        "Open this official OpenAI URL in a browser on this computer:\n{}",
        pending.authorization_url()
    );
    let cancel = CancellationToken::new();
    let interrupt = cancel.clone();
    let signal = tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        interrupt.cancel();
    });
    let result = pending.complete(&cancel).await;
    signal.abort();
    let status = result?;
    anyhow::ensure!(
        status.plan_enabled,
        "ChatGPT sign-in is saved; enable plan access with by-human login --enable-plan"
    );
    println!("by-human: ChatGPT login is ready");
    Ok(())
}

async fn auth_status() -> anyhow::Result<()> {
    let auth = AuthManager::new(Config::auth_directory()?)?;
    let status = auth.status().await?;
    println!(
        "by-human: registration_saved={}, authenticated={}, plan_enabled={}, expires_at={}",
        status.registration_saved,
        status.authenticated,
        status.plan_enabled,
        status
            .expires_at
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".into())
    );
    Ok(())
}

/// Discovery needs only the explicit MCP configuration, not model credentials
/// or a database. No tool is called by this command.
async fn mcp_list() -> anyhow::Result<()> {
    let config = McpConfig::from_env()?;
    let cancel = CancellationToken::new();
    let mut connections = tokio::select! {
        result = McpConnections::connect(&config, &cancel) => result?,
        _ = shutdown_signal() => {
            cancel.cancel();
            anyhow::bail!("MCP discovery was cancelled");
        }
    };
    println!("{}", serde_json::json!({"tools":connections.tools()}));
    connections.shutdown().await?;
    Ok(())
}

async fn serve() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let mcp_config = McpConfig::from_env()?;
    let mut auth_manager = None;
    let model: Arc<dyn Model> = match config.openai_auth {
        AuthMode::ChatGptPlan => {
            let auth = AuthManager::new(Config::auth_directory()?)?;
            let status = auth.status().await?;
            anyhow::ensure!(status.authenticated, "run by-human login first");
            anyhow::ensure!(
                status.plan_enabled,
                "enable ChatGPT plan access with by-human login --enable-plan"
            );
            let model = ChatGptModel::new(auth.clone(), config.model)?;
            auth_manager = Some(auth);
            Arc::new(model)
        }
        AuthMode::ApiKey => Arc::new(OpenAIProvider::new(ProviderConfig::new(
            config.model,
            config
                .openai_api_key
                .expect("validated explicit API-key mode"),
        ))?),
    };
    let store = Store::connect(&config.database_url)
        .await
        .map_err(|_| anyhow::anyhow!("PostgreSQL could not be initialized"))?;
    let shutdown = CancellationToken::new();
    // Register termination while MCP starts, so an interrupted handshake also
    // drops its owned process/connection instead of leaving an orphan.
    let signal = shutdown_signal();
    tokio::pin!(signal);
    let mut connections = tokio::select! {
        result = McpConnections::connect(&mcp_config, &shutdown) => result?,
        _ = &mut signal => {
            shutdown.cancel();
            anyhow::bail!("server startup was cancelled");
        }
    };
    let mut tools = ToolRegistry::default();
    connections.register(&mut tools)?;
    let engine = Arc::new(Engine::with_tools(
        model,
        Limits::default(),
        Arc::new(tools),
    ));
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|_| anyhow::anyhow!("HTTP listener could not be bound"))?;
    let listening_at = listener
        .local_addr()
        .map_err(|_| anyhow::anyhow!("HTTP listener address is unavailable"))?;
    let runner = Runner::start(store.clone(), engine, shutdown.clone()).await?;
    let fatal = runner.fatal_token();
    let state = ApiState::new(store, config.api_token, runner.notifier())
        .map_err(|message| anyhow::anyhow!("{message}"))?;
    let http_shutdown = shutdown.clone();
    let mut http = tokio::spawn(async move {
        axum::serve(listener, api::router(state))
            .with_graceful_shutdown(http_shutdown.cancelled_owned())
            .await
    });
    println!("by-human: HTTP API is ready at {listening_at}");
    let mut http_finished = false;
    let mut failed = tokio::select! {
        result = &mut signal => result.is_err(),
        _ = fatal.cancelled() => true,
        _ = &mut http => { http_finished = true; true },
    };
    shutdown.cancel();
    if runner.shutdown().await.is_err() {
        failed = true;
    }
    if connections.shutdown().await.is_err() {
        failed = true;
    }
    // SSE clients can retain a connection indefinitely. Durability is settled
    // by the runner before those sockets are closed during a bounded shutdown.
    if !http_finished {
        match tokio::time::timeout(Duration::from_secs(3), &mut http).await {
            Ok(Ok(Ok(()))) => {}
            Ok(_) => failed = true,
            Err(_) => {
                http.abort();
                let _ = http.await;
            }
        }
    }
    if let Some(auth) = auth_manager {
        // Finish any already accepted refresh rotation even if Stop cancelled
        // its model caller. The auth transport bounds this settlement.
        if auth.settle_refreshes().await.is_err() {
            failed = true;
        }
    }
    anyhow::ensure!(!failed, "server stopped because runtime readiness was lost");
    Ok(())
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
