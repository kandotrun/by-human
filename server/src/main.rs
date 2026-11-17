use std::{sync::Arc, time::Duration};

use by_human::{
    api::{self, ApiState},
    config::Config,
    engine::{Engine, Limits, OpenAIProvider, ProviderConfig},
    runner::Runner,
    store::Store,
};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let command = std::env::args().nth(1);
    match command.as_deref() {
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
                "Usage: by-human <check|serve>\n\ncheck  Apply PostgreSQL migrations and check readiness.\nserve  Start the authenticated HTTP API and single-owner runner."
            );
            std::process::ExitCode::SUCCESS
        }
        Some(_) => {
            eprintln!("by-human: unknown command; use --help");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn serve() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let store = Store::connect(&config.database_url)
        .await
        .map_err(|_| anyhow::anyhow!("PostgreSQL could not be initialized"))?;
    let provider = OpenAIProvider::new(ProviderConfig::new(config.model, config.openai_api_key))?;
    let engine = Arc::new(Engine::new(Arc::new(provider), Limits::default()));
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|_| anyhow::anyhow!("HTTP listener could not be bound"))?;
    let shutdown = CancellationToken::new();
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
    println!("by-human: HTTP API is ready at {}", config.listen);
    let mut http_finished = false;
    let mut failed = tokio::select! {
        result = shutdown_signal() => result.is_err(),
        _ = fatal.cancelled() => true,
        _ = &mut http => { http_finished = true; true },
    };
    shutdown.cancel();
    if runner.shutdown().await.is_err() {
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
