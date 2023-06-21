use anyhow::anyhow;
use anyhow::Context;
use dropshot::ConfigLogging;
use dropshot::ConfigLoggingIfExists;
use dropshot::ConfigLoggingLevel;
use slog::info;

mod http;
mod model;
mod pool;
mod schema;

type DbConnection = diesel::PgConnection;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db_url = "postgresql://root@127.0.0.1:12345/defaultdb?sslmode=disable";
    let config_logging = ConfigLogging::File {
        level: ConfigLoggingLevel::Debug,
        path: "/dev/stdout".into(),
        if_exists: ConfigLoggingIfExists::Append,
    };
    let config_dropshot = dropshot::ConfigDropshot {
        bind_address: "127.0.0.1:12344".parse().unwrap(),
        ..Default::default()
    };
    let log =
        config_logging.to_logger("cancel-repro").context("creating logger")?;
    let pool = pool::create_pool(log.clone(), db_url)
        .await
        .context("setting up database pool")?;
    info!(&log, "setting up dropshot server");
    let server =
        http::create_dropshot_server(config_dropshot, log.clone(), pool)
            .await?;
    info!(&log, "set up dropshot server";
        "local_address" => ?server.local_addr());
    server.await.map_err(|error| anyhow!("waiting for server: {:#}", error))
}
