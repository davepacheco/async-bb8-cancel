use crate::model::Counter;
use crate::schema;
use crate::DbConnection;
use anyhow::anyhow;
use anyhow::Context;
use async_bb8_diesel::AsyncConnection;
use async_bb8_diesel::AsyncRunQueryDsl;
use async_bb8_diesel::AsyncSimpleConnection;
use diesel::prelude::*;
use dropshot::endpoint;
use dropshot::ApiDescription;
use dropshot::ConfigDropshot;
use dropshot::HttpError;
use dropshot::HttpResponseOk;
use dropshot::HttpResponseUpdatedNoContent;
use dropshot::HttpServerStarter;
use dropshot::RequestContext;
use slog::error;
use slog::info;

/// The server-wide context consists of just the database connection pool
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

/// Sleep a bit in a transaction
#[endpoint {
    method = GET,
    path = "/sleep",
}]
async fn api_sleep(
    rqctx: RequestContext<ExampleContext>,
) -> Result<HttpResponseOk<&'static str>, HttpError> {
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
