mod protocol;
mod routes;

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use localflake_engine::Engine;

/// A local, DuckDB-backed emulator for the Snowflake driver protocol.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Address to listen on.
    #[arg(long, env = "LOCALFLAKE_ADDR", default_value = "127.0.0.1:8080")]
    addr: SocketAddr,

    /// Directory for persistent databases (one DuckDB file each). In-memory if unset.
    #[arg(long, env = "LOCALFLAKE_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Result encoding: `auto` uses Arrow for drivers that support it (as Snowflake does).
    #[arg(long, env = "LOCALFLAKE_RESULT_FORMAT", default_value = "auto", value_parser = ["auto", "json", "arrow"])]
    result_format: String,

    /// Logging in with a database that doesn't exist: `create` it (convenient
    /// for tests), or `ignore` it and log in without a current database, as
    /// Snowflake does.
    #[arg(long, env = "LOCALFLAKE_MISSING_DATABASE", default_value = "create", value_parser = ["create", "ignore"])]
    missing_database: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "localflake=info,localflake_engine=info".into()),
        )
        .init();

    let args = Args::parse();
    let engine = Engine::open(args.data_dir.as_deref())?;
    engine.set_create_missing_databases(args.missing_database == "create");
    let listener = tokio::net::TcpListener::bind(args.addr).await?;
    tracing::info!(addr = %listener.local_addr()?, data_dir = ?args.data_dir, "localflake listening");

    let forced_format = match args.result_format.as_str() {
        "json" => Some(protocol::ResultFormat::Json),
        "arrow" => Some(protocol::ResultFormat::Arrow),
        _ => None,
    };
    axum::serve(listener, routes::router(engine, forced_format))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
