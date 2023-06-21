use crate::DbConnection;
use anyhow::Context;
use slog::error;

pub async fn create_pool(
    log: slog::Logger,
    db_url: &str,
) -> anyhow::Result<bb8::Pool<async_bb8_diesel::ConnectionManager<DbConnection>>>
{
    let error_sink = LoggingErrorSink::new(log.clone());
    let manager = async_bb8_diesel::ConnectionManager::new(db_url);
    bb8::Builder::new()
        .error_sink(Box::new(error_sink))
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
