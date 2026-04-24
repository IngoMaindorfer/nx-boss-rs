use axum::{
    extract::{Multipart, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde_json::json;

use crate::state::AppState;

pub async fn post_image(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let mut image_filename: Option<String> = None;
    let mut image_bytes: Option<Vec<u8>> = None;
    let mut parameter_bytes: Option<Vec<u8>> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        match field.name() {
            Some("image") => {
                image_filename = field.file_name().map(|s| s.to_string());
                match field.bytes().await {
                    Ok(b) => image_bytes = Some(b.to_vec()),
                    Err(e) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({ "error": e.to_string() })),
                        )
                            .into_response()
                    }
                }
            }
            Some("parameter") => match field.bytes().await {
                Ok(b) => parameter_bytes = Some(b.to_vec()),
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": e.to_string() })),
                    )
                        .into_response()
                }
            },
            // imageparameter field is accepted but not used
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    let filename = image_filename.unwrap_or_else(|| "image".to_string());
    let content = match image_bytes {
        Some(b) => b,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "missing image field" })),
            )
                .into_response()
        }
    };
    let parameters: serde_json::Value = match parameter_bytes {
        Some(b) => match serde_json::from_slice(&b) {
            Ok(v) => v,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": format!("invalid parameter JSON: {e}") })),
                )
                    .into_response()
            }
        },
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "missing parameter field" })),
            )
                .into_response()
        }
    };

    let batch_id = match parameters["batch_id"].as_str() {
        Some(id) => id.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "missing batch_id in parameter" })),
            )
                .into_response()
        }
    };

    let mut batches = state.batches.lock().unwrap();
    let batch = match batches.get_mut(&batch_id) {
        Some(b) => b,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    match batch.add_file(&filename, &content, parameters) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) if e.to_string().contains("bad filename") => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "bad filename" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
