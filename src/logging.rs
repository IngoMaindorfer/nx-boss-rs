use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use crate::event_log::{EventLayer, EventLog};

fn allowed_level(value: Option<&str>) -> &'static str {
    match value {
        Some("error") => "error",
        Some("warn") => "warn",
        Some("info") => "info",
        Some("debug") => "debug",
        _ => "info",
    }
}

pub fn init() -> EventLog {
    let requested = std::env::var("NX_BOSS_LOG_LEVEL").ok();
    let level = allowed_level(requested.as_deref());
    let event_log = EventLog::default();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(format!("nx_boss_rs={level}")))
        .json()
        .flatten_event(true)
        .with_current_span(true)
        .with_span_list(false)
        .with_ansi(false)
        .finish()
        .with(EventLayer(event_log.clone()))
        .init();

    std::panic::set_hook(Box::new(|panic| {
        let (file, line) = panic
            .location()
            .map(|location| (location.file(), location.line()))
            .unwrap_or(("unknown", 0));
        tracing::error!(event = "process.panic", file, line, "process panicked");
    }));

    if requested.as_deref().is_some_and(|value| value != level) {
        tracing::warn!(
            event = "logging.invalid_level",
            "NX_BOSS_LOG_LEVEL must be error, warn, info, or debug; using info"
        );
    }
    event_log
}

pub fn error_class(error: &anyhow::Error) -> &'static str {
    for cause in error.chain() {
        if cause.is::<crate::paperless::PaperlessHttpStatus>() {
            return "http_status";
        }
        if cause.is::<std::io::Error>() {
            return "io";
        }
        if cause.is::<reqwest::Error>() {
            return "http";
        }
        if cause.is::<serde_json::Error>() {
            return "json";
        }
        if cause.is::<serde_yaml::Error>() {
            return "yaml";
        }
    }
    "other"
}

pub fn http_status(error: &anyhow::Error) -> u16 {
    for cause in error.chain() {
        if let Some(status) = cause.downcast_ref::<crate::paperless::PaperlessHttpStatus>() {
            return status.0.as_u16();
        }
        if let Some(request) = cause.downcast_ref::<reqwest::Error>()
            && let Some(status) = request.status()
        {
            return status.as_u16();
        }
    }
    0
}

pub fn safe_batch_id(id: &str) -> &str {
    if crate::batch::valid_batch_id(id) {
        id
    } else {
        "<invalid>"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_level_is_an_allowlist_for_application_events() {
        assert_eq!(allowed_level(Some("debug")), "debug");
        assert_eq!(allowed_level(Some("warn")), "warn");
        assert_eq!(allowed_level(None), "info");
        assert_eq!(allowed_level(Some("trace")), "info");
        assert_eq!(allowed_level(Some("debug,reqwest=trace")), "info");
    }

    #[test]
    fn only_generated_batch_id_shape_is_logged() {
        assert_eq!(
            safe_batch_id("67d9f0a41234567890abcdef12345678"),
            "67d9f0a41234567890abcdef12345678"
        );
        assert_eq!(safe_batch_id("PRIVATE_BODY_VALUE"), "<invalid>");
    }

    #[test]
    fn error_class_never_contains_the_raw_error() {
        let error = anyhow::anyhow!("request failed: Token PRIVATE_API_TOKEN at /private/path");
        assert_eq!(error_class(&error), "other");
        let io_error = anyhow::Error::new(std::io::Error::other("PRIVATE_API_TOKEN"));
        assert_eq!(error_class(&io_error), "io");
    }

    #[test]
    fn paperless_http_failures_keep_only_numeric_status() {
        let error = anyhow::Error::new(crate::paperless::PaperlessHttpStatus(
            reqwest::StatusCode::UNAUTHORIZED,
        ));
        assert_eq!(error_class(&error), "http_status");
        assert_eq!(http_status(&error), 401);
    }
}
