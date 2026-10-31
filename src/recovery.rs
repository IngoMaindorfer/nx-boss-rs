use crate::batch::BatchMetadata;
use chrono::{DateTime, Utc};
use std::path::Path;

#[derive(Debug)]
pub struct Permissions {
    pub complete: bool,
    pub discard: bool,
}

pub fn permissions(
    meta: &BatchMetadata,
    corrupt: bool,
    dir: &Path,
    now: DateTime<Utc>,
) -> Permissions {
    let activity = if corrupt {
        dir.symlink_metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .map(DateTime::<Utc>::from)
    } else {
        DateTime::parse_from_rfc3339(&meta.created_at)
            .ok()
            .map(|t| t.with_timezone(&Utc))
            .and_then(|created| {
                meta.files.iter().try_fold(created, |latest, page| {
                    DateTime::parse_from_rfc3339(&page.received_at)
                        .ok()
                        .map(|t| latest.max(t.with_timezone(&Utc)))
                })
            })
    };
    let idle =
        activity.is_some_and(|last| now.signed_duration_since(last) >= chrono::Duration::hours(1));
    let delivered = meta.completed
        && meta
            .paperless
            .as_ref()
            .is_some_and(|r| r.document_id.is_some());
    Permissions {
        complete: !meta.completed && !corrupt && idle,
        discard: !delivered && (corrupt || (!meta.completed && idle)),
    }
}
