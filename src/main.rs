//! Demos a failure resulting from cancel-unsafe code
//!
//! This program connects to a PostgreSQL/CockroachDB database and stores a
//! counter there.  The program exposes an HTTP server to fetch and bump the
//! counter.  See the README for details.

use anyhow::anyhow;
use anyhow::Context;
use async_bb8_diesel::AsyncConnection;
use async_bb8_diesel::AsyncRunQueryDsl;
use async_bb8_diesel::AsyncSimpleConnection;
use diesel::prelude::*;
use dropshot::endpoint;
use dropshot::ApiDescription;
use dropshot::ConfigDropshot;
use dropshot::ConfigLogging;
use dropshot::ConfigLoggingIfExists;
use dropshot::ConfigLoggingLevel;
use dropshot::HttpError;
use dropshot::HttpResponseOk;
use dropshot::HttpResponseUpdatedNoContent;
use dropshot::HttpServerStarter;
use dropshot::RequestContext;
use slog::error;
use slog::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Configuration
    let db_url = "postgresql://root@127.0.0.1:12345/defaultdb?sslmode=disable";
    let config_dropshot = dropshot::ConfigDropshot {
        bind_address: "127.0.0.1:12344".parse().unwrap(),
        ..Default::default()
    };
    let config_logging = ConfigLogging::File {
        level: ConfigLoggingLevel::Debug,
        path: "/dev/stdout".into(),
        if_exists: ConfigLoggingIfExists::Append,
    };

    // Set up logging.
    let log =
        config_logging.to_logger("cancel-repro").context("creating logger")?;

    // Set up a bb8 connection pool for the database.
    let pool = create_pool(log.clone(), db_url)
        .await
        .context("setting up database pool")?;

    // Set up the Dropshot server.
    info!(&log, "setting up dropshot server");
    let server =
        create_dropshot_server(config_dropshot, log.clone(), pool).await?;
    info!(&log, "set up dropshot server";
        "local_address" => ?server.local_addr());

    // Wait forever serving HTTP requests.
    server.await.map_err(|error| anyhow!("waiting for server: {:#}", error))
}

///////////////////////////////
// Database model and schema
///////////////////////////////

// We've got one database table called "counter" with just two integer fields:
// an id and a count.

#[derive(Queryable, Selectable)]
#[diesel(table_name = schema::counter)]
pub struct Counter {
    pub id: i32,
    pub count: i32,
}

mod schema {
    diesel::table! {
        counter (id) {
            id -> Int4,
            count -> Int4,
        }
    }
}

///////////////////////////////
// Dropshot server
///////////////////////////////

type DbConnection = diesel::PgConnection;

/// Construct our Dropshot server
pub async fn create_dropshot_server(
    config_dropshot: ConfigDropshot,
    log: slog::Logger,
    pool: bb8::Pool<async_bb8_diesel::ConnectionManager<DbConnection>>,
) -> anyhow::Result<dropshot::HttpServer<ExampleContext>> {
    let mut api = ApiDescription::new();
    api.register(api_reset).unwrap();
    api.register(api_get_counter).unwrap();
    api.register(api_bump_counter).unwrap();
    api.register(api_sleep).unwrap();

    let api_context = ExampleContext::new(pool);

    Ok(HttpServerStarter::new(&config_dropshot, api, api_context, &log)
        .map_err(|error| anyhow!("creating Dropshot server: {:#}", error))?
        .start())
}

/// Our Dropshot context consists of just the database connection pool
pub struct ExampleContext {
    pool: bb8::Pool<async_bb8_diesel::ConnectionManager<DbConnection>>,
}

impl ExampleContext {
    fn new(
        pool: bb8::Pool<async_bb8_diesel::ConnectionManager<DbConnection>>,
    ) -> ExampleContext {
        ExampleContext { pool }
    }
}

/// Wraps anyhow::Error so that we can serialize it as an error from our
/// Dropshot endpoints
struct ErrorWrap(anyhow::Error);
impl From<ErrorWrap> for HttpError {
    fn from(value: ErrorWrap) -> Self {
        let message = format!("{:#}", value.0);
        dropshot::HttpError {
            status_code: http::StatusCode::INTERNAL_SERVER_ERROR,
            error_code: None,
            external_message: message.clone(),
            internal_message: message,
        }
    }
}

/// Wipe the database and start again
#[endpoint {
    method = POST,
    path = "/reset",
}]
async fn api_reset(
    rqctx: RequestContext<ExampleContext>,
) -> Result<HttpResponseOk<&'static str>, HttpError> {
    let api_context = rqctx.context();
    let pool = &api_context.pool;
    pool.batch_execute_async(
        r#"
        DROP TABLE IF EXISTS counter;
        CREATE TABLE counter (id INT4, count INT4);
        INSERT INTO counter (id, count) VALUES (1, 1);
        "#,
    )
    .await
    .context("setting up database")
    .map_err(ErrorWrap)?;
    Ok(HttpResponseOk("ok"))
}

/// Fetch the current value of the counter.
#[endpoint {
    method = GET,
    path = "/counter",
}]
async fn api_get_counter(
    rqctx: RequestContext<ExampleContext>,
) -> Result<HttpResponseOk<i32>, HttpError> {
    let api_context = rqctx.context();
    let pool = &api_context.pool;
    // SELECT FROM counter WHERE id = 1
    let value = schema::counter::dsl::counter
        .filter(schema::counter::dsl::id.eq(1))
        .select(Counter::as_select())
        .first_async(pool)
        .await
        .context("loading counter")
        .map_err(ErrorWrap)?;
    Ok(HttpResponseOk(value.count))
}

/// Bump the current value of the counter.
#[endpoint {
    method = POST,
    path = "/bump",
}]
async fn api_bump_counter(
    rqctx: RequestContext<ExampleContext>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    let api_context = rqctx.context();
    let pool = &api_context.pool;
    // UPDATE counter SET count = count + 1 WHERE id = 1
    let nrows = diesel::update(
        schema::counter::dsl::counter.filter(schema::counter::dsl::id.eq(1)),
    )
    .set(schema::counter::dsl::count.eq(schema::counter::dsl::count + 1))
    .execute_async(pool)
    .await
    .context("executing update")
    .map_err(ErrorWrap)?;
    info!(rqctx.log, "bumped counter"; "nrows_updated" => nrows);
    Ok(HttpResponseUpdatedNoContent())
}

/// Sleep a bit inside a database transaction
#[endpoint {
    method = GET,
    path = "/sleep",
}]
async fn api_sleep(
    rqctx: RequestContext<ExampleContext>,
) -> Result<HttpResponseOk<&'static str>, HttpError> {
    // This guard is used to log a message when this task gets cancelled.
    struct Finished<'a>(&'a slog::Logger, bool);
    impl<'a> Drop for Finished<'a> {
        fn drop(&mut self) {
            if !self.1 {
                error!(&self.0, "api_sleep() cancelled");
            }
        }
    }

    let mut finished = Finished(&rqctx.log, false);
    let api_context = rqctx.context();
    let pool = &api_context.pool;
    // Enter a transaction, do an async operation, and then return.
    let result: Result<_, anyhow::Error> = pool
        .transaction_async(|_conn| async {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            Ok(())
        })
        .await;
    let _ = result.context("executing transaction").map_err(ErrorWrap)?;
    finished.1 = true;

    Ok(HttpResponseOk("ok"))
}

///////////////////////////////
// Database connection pool
///////////////////////////////

pub async fn create_pool(
    log: slog::Logger,
    db_url: &str,
) -> anyhow::Result<bb8::Pool<async_bb8_diesel::ConnectionManager<DbConnection>>>
{
    let error_sink = LoggingErrorSink::new(log.clone());
    let manager = async_bb8_diesel::ConnectionManager::new(db_url);
    bb8::Builder::new()
        .error_sink(Box::new(error_sink))
        // Use at most one connection to make sure we reliably get the broken
        // one after we trigger the bug.
        .max_size(1)
        .build(manager)
        .await
        .context("building pool")
}

// LoggingErrorSink is cribbed from Omicron
#[derive(Clone, Debug)]
struct LoggingErrorSink {
    log: slog::Logger,
}

impl LoggingErrorSink {
    fn new(log: slog::Logger) -> LoggingErrorSink {
        LoggingErrorSink { log }
    }
}

impl bb8::ErrorSink<async_bb8_diesel::ConnectionError> for LoggingErrorSink {
    fn sink(&self, error: async_bb8_diesel::ConnectionError) {
        error!(
            &self.log,
            "database connection error";
            "error_message" => #%error
        );
    }

    fn boxed_clone(
        &self,
    ) -> Box<dyn bb8::ErrorSink<async_bb8_diesel::ConnectionError>> {
        Box::new(self.clone())
    }
}
