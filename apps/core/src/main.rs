//! Entry point for the MyOTA identity and programme service.
//!
//! Two modes:
//!
//! ```text
//! core                                   serve HTTP
//! core provision-admin --email … --name … --password-stdin
//! core --version
//! ```
//!
//! Provisioning lives in the binary rather than in a migration because it needs the
//! password hasher and the MFA encryption key — the two things that make a
//! provisioned account usable. A SQL script would have to embed a hash and a sealed
//! seed, which is a credential in a file.

use std::error::Error;
use std::io::Read as _;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use myota_core::config::Config;
use myota_core::http::{router, AppState};
use myota_core::token::TokenService;
use myota_core::{bootstrap, db, SERVICE_NAME};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // The message is the operator's; the chain is for the log.
            eprintln!("{SERVICE_NAME}: {error}");
            let mut source = error.source();
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    match arguments.first().map(String::as_str) {
        Some("--version") => {
            println!("{SERVICE_NAME} {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("provision-admin") => provision_admin(&arguments[1..]).await,
        Some("serve") | None => serve().await,
        Some(other) => Err(format!(
            "{other:?} is not a command; use `core`, `core provision-admin` or `core --version`"
        )
        .into()),
    }
}

/// Serves until a signal, then drains.
async fn serve() -> Result<(), Box<dyn Error>> {
    initialise_tracing();

    let config = Config::from_env()?;
    tracing::info!(
        listen_addr = %config.listen_addr,
        otp_issuer = %config.otp_issuer,
        "starting"
    );

    // Failing to reach the database is fatal at startup. A replica that starts
    // without one passes its liveness probe and fails every request; refusing here
    // makes the deployment's first event the true one.
    let pool = db::connect(&config.database_url).await?;

    let tokens = Arc::new(TokenService::new(
        &config.token_seed,
        config.token_key_id.clone(),
        config.access_ttl,
    ));
    let state = AppState::new(pool.clone(), Arc::new(config.clone()), tokens);
    let app = router(state);

    let listener = tokio::net::TcpListener::bind(&config.listen_addr).await?;
    tracing::info!("listening on {}", listener.local_addr()?);

    // `into_make_service_with_connect_info` is what gives the handler the peer
    // address, which the rate limiter uses when `X-Forwarded-For` is not trusted.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    tracing::info!("stopped");
    Ok(())
}

/// Creates or updates an administrator and enrols a TOTP factor.
async fn provision_admin(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    initialise_tracing();
    let config = Config::from_env()?;

    let flags = bootstrap::parse_provision_args(arguments)?;

    let password = match flags.password {
        // From stdin rather than argv: an argument is visible in `ps` and in a shell
        // history, and this is a credential.
        bootstrap::PasswordSource::Stdin => {
            let mut buffer = String::new();
            std::io::stdin().read_to_string(&mut buffer)?;
            buffer.trim_end_matches(['\n', '\r']).to_string()
        }
        bootstrap::PasswordSource::Literal(value) => value,
    };

    let pool = db::connect(&config.database_url).await?;
    let provisioned =
        bootstrap::provision_admin(&pool, &config, &flags.email, &flags.display_name, &password)
            .await?;

    print!("{}", bootstrap::render(&provisioned, &flags.email));
    Ok(())
}

/// JSON logs, level from `RUST_LOG`.
///
/// JSON because the cluster's logging stack reads it and a human greps it rarely;
/// `RUST_LOG=info,myota_core=debug` raises one component without flooding the rest.
fn initialise_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_current_span(false)
        .init();
}

/// Resolves on SIGTERM or Ctrl-C.
///
/// Kubernetes sends SIGTERM and waits; the server stops accepting, finishes in-flight
/// requests, and the process exits. A container that ignores it gets SIGKILLed and
/// drops requests, which is the difference between a rollout nobody notices and a
/// burst of 502s.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "could not install the SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutting down");
}
