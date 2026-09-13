use anyhow::Result;
use duckxy::auth::Auth;
use duckxy::dataset::DatasetRoot;
use duckxy::{AppState, Config, VERSION, grammar, query, routes};
use tokio::net::TcpListener;
use tokio::signal;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("install Ctrl-C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate()).expect("install SIGTERM handler").recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    info!("shutdown signal received");
}

fn sign_path(config: &Config, path: &str) -> Result<()> {
    if !path.starts_with('/') {
        anyhow::bail!("path must start with '/', got: {path}");
    }
    let Some(key) = config.key.as_deref() else {
        anyhow::bail!("DUCKXY_KEY is required to sign");
    };
    let signature = Auth::new(Some(key), false)?.sign(path).expect("a key was supplied");
    println!("/{signature}{path}");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env();

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "sign") {
        let Some(path) = args.get(2) else {
            anyhow::bail!(
                "usage: DUCKXY_KEY=<hex> duckxy sign /@dataset:name/output.geojson\n\
                 via cargo: DUCKXY_KEY=<hex> cargo run -- sign /@dataset:name/output.geojson"
            );
        };
        return sign_path(&config, path);
    }

    fmt().with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))).init();

    let auth = Auth::new(config.key.as_deref(), config.allow_insecure)?;
    let root = DatasetRoot::new(config.data_root);

    query::install_extensions()?;

    let listener = TcpListener::bind(&config.bind).await?;
    info!(
        bind = %config.bind,
        version = VERSION,
        data_root = %root.root().display(),
        allow_insecure = config.allow_insecure,
        "duckxy starting"
    );

    axum::serve(
        listener,
        routes::router(AppState { root, auth, grammar: std::sync::Arc::new(grammar::Grammar::core()) }),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}
