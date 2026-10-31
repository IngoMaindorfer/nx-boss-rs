use anyhow::{Context, Result};

pub fn configured_timezone() -> Result<chrono_tz::Tz> {
    std::env::var("NX_BOSS_TIMEZONE")
        .unwrap_or_else(|_| "Europe/Berlin".into())
        .parse()
        .context("invalid NX_BOSS_TIMEZONE")
}

pub fn local_scan_date(created_at: &str, timezone: chrono_tz::Tz) -> Result<String> {
    Ok(chrono::DateTime::parse_from_rfc3339(created_at)?
        .with_timezone(&timezone)
        .format("%Y-%m-%d")
        .to_string())
}
