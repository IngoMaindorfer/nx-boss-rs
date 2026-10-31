use axum::{
    Json,
    extract::{Multipart, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::batch::{PathTraversalError, UploadError};
use crate::lock;
use crate::state::AppState;

struct ParsedImage {
    filename: String,
    content: Vec<u8>,
    parameters: Value,
}

fn bad_request(msg: impl std::fmt::Display) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": msg.to_string() })),
    )
        .into_response()
}

async fn parse_multipart(mut multipart: Multipart) -> Result<ParsedImage, Box<Response>> {
    fn multipart_error(message: impl std::fmt::Display) -> Box<Response> {
        Box::new(bad_request(message))
    }

    let mut image_filename: Option<String> = None;
    let mut image_bytes: Option<Vec<u8>> = None;
    let mut parameter_bytes: Option<Vec<u8>> = None;

    while let Some(field) = multipart.next_field().await.map_err(multipart_error)? {
        match field.name() {
            Some("image") => {
                image_filename = field.file_name().map(|s| s.to_string());
                match field.bytes().await {
                    Ok(b) => image_bytes = Some(b.to_vec()),
                    Err(e) => {
                        warn!(
                            event = "image.multipart_failed",
                            "failed to read image field"
                        );
                        return Err(multipart_error(e));
                    }
                }
            }
            Some("parameter") => match field.bytes().await {
                Ok(b) => parameter_bytes = Some(b.to_vec()),
                Err(e) => {
                    warn!(
                        event = "image.multipart_failed",
                        "failed to read parameter field"
                    );
                    return Err(multipart_error(e));
                }
            },
            _ => {
                field.bytes().await.map_err(multipart_error)?;
            }
        }
    }

    let filename = image_filename.unwrap_or_else(|| "image".to_string());
    let content = match image_bytes {
        Some(b) => b,
        None => {
            warn!(
                event = "image.missing_field",
                "post_image: missing image field"
            );
            return Err(multipart_error("missing image field"));
        }
    };
    let parameters: Value = match parameter_bytes {
        Some(b) => match serde_json::from_slice(&b) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    event = "image.invalid_parameters",
                    "post_image: invalid parameter JSON"
                );
                return Err(multipart_error(format!("invalid parameter JSON: {e}")));
            }
        },
        None => {
            warn!(
                event = "image.missing_field",
                "post_image: missing parameter field"
            );
            return Err(multipart_error("missing parameter field"));
        }
    };

    Ok(ParsedImage {
        filename,
        content,
        parameters,
    })
}

pub async fn post_image(State(state): State<AppState>, multipart: Multipart) -> impl IntoResponse {
    let Ok(_slot) = state.upload_slots.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let parsed = match parse_multipart(multipart).await {
        Ok(p) => p,
        Err(r) => return *r,
    };

    let batch_id = match parsed.parameters["batch_id"].as_str() {
        Some(id) => id.to_string(),
        None => {
            warn!(
                event = "image.missing_batch_id",
                "post_image: missing batch_id in parameter JSON"
            );
            return bad_request("missing batch_id in parameter");
        }
    };

    if !crate::pdf::valid_jpeg(&parsed.content) {
        warn!(event = "image.invalid_format", "post_image: not a JPEG");
        return bad_request("not a JPEG image");
    }

    let batch = match lock!(state.batches).get(&batch_id).cloned() {
        Some(b) => b,
        None => {
            warn!(
                event = "image.unknown_batch",
                "post_image: unknown batch_id"
            );
            return StatusCode::NOT_FOUND.into_response();
        }
    };

    let filename = parsed.filename.clone();
    let bytes = parsed.content.len();
    let content = parsed.content;
    let parameters = parsed.parameters;
    // add_file does file I/O — run off the async thread.
    let result = tokio::task::spawn_blocking(move || {
        let mut batch = lock!(batch);
        batch.add_file(&filename, &content, parameters)
    })
    .await;

    match result {
        Ok(Ok(())) => {
            info!(
                event = "image.saved",
                batch_id = %crate::logging::safe_batch_id(&batch_id),
                bytes,
                "image saved"
            );
            StatusCode::OK.into_response()
        }
        Ok(Err(e)) if e.downcast_ref::<PathTraversalError>().is_some() => {
            warn!(event = "image.invalid_filename", batch_id = %crate::logging::safe_batch_id(&batch_id), "post_image: path traversal rejected");
            bad_request("bad filename")
        }
        Ok(Err(e))
            if matches!(
                e.downcast_ref::<UploadError>(),
                Some(UploadError::LimitExceeded)
            ) =>
        {
            StatusCode::PAYLOAD_TOO_LARGE.into_response()
        }
        Ok(Err(e)) if e.downcast_ref::<UploadError>().is_some() => {
            StatusCode::CONFLICT.into_response()
        }
        Ok(Err(e)) => {
            warn!(event = "image.save_failed", batch_id = %crate::logging::safe_batch_id(&batch_id), error_class = crate::logging::error_class(&e), "post_image: failed to save image");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to save image" })),
            )
                .into_response()
        }
        Err(_) => {
            tracing::error!(event = "image.worker_failed", batch_id = %crate::logging::safe_batch_id(&batch_id), "image upload worker failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::config::{Config, Job};
    use crate::routes::test_support::TestApp;
    use crate::state::AppState;

    fn test_server() -> (TestApp, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            jobs: vec![Job {
                output_path: tmp.path().to_path_buf(),
                consume_path: None,
                job_info: json!({
                    "name": "test", "job_id": 0, "color": "#fff",
                    "type": 0, "job_setting": {}, "hierarchy_list": null
                }),
                scan_settings: json!({}),
            }],
            ..Default::default()
        };
        let server = TestApp::new(AppState::new(config));
        (server, tmp)
    }

    /// Build a raw multipart/form-data body with one image field and one parameter field.
    fn multipart_body(boundary: &str, image: &[u8], filename: &str, param_json: &str) -> Vec<u8> {
        let mut body = Vec::new();
        let sep = format!("--{boundary}\r\n");
        let end = format!("--{boundary}--\r\n");

        body.extend_from_slice(sep.as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"image\"; filename=\"{filename}\"\r\n\
                 Content-Type: image/jpeg\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(image);
        body.extend_from_slice(b"\r\n");

        body.extend_from_slice(sep.as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"parameter\"\r\n\r\n");
        body.extend_from_slice(param_json.as_bytes());
        body.extend_from_slice(b"\r\n");

        body.extend_from_slice(end.as_bytes());
        body
    }

    async fn create_batch(server: &TestApp) -> String {
        let resp = server
            .post("/NmWebService/batch")
            .json(&json!({"job_id": 0}))
            .await;
        assert_eq!(resp.status_code(), 200);
        resp.json::<Value>()["batch_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Baseline: standard multipart/form-data upload must succeed.
    #[tokio::test]
    async fn test_image_upload_form_data_ok() {
        let (server, _tmp) = test_server();
        let batch_id = create_batch(&server).await;
        let boundary = "formboundary123";
        let param = json!({"batch_id": batch_id}).to_string();
        let body = multipart_body(
            boundary,
            include_bytes!("../../tests/fixtures/scan_page.jpg"),
            "p.jpg",
            &param,
        );

        let resp = server
            .post("/NmWebService/image")
            .content_type(&format!("multipart/form-data; boundary={boundary}"))
            .bytes(body.into())
            .await;
        assert_eq!(resp.status_code(), 200);
    }

    #[tokio::test]
    async fn test_truncated_multipart_does_not_save_page() {
        let (server, tmp) = test_server();
        let batch_id = create_batch(&server).await;
        let boundary = "truncatedboundary";
        let param = json!({"batch_id": batch_id}).to_string();
        let mut body = multipart_body(
            boundary,
            include_bytes!("../../tests/fixtures/scan_page.jpg"),
            "p.jpg",
            &param,
        );
        body.truncate(body.len() - (boundary.len() + 6));
        body.extend_from_slice(format!("--{boundary}\r\nmalformed header\r\n\r\n").as_bytes());
        let resp = server
            .post("/NmWebService/image")
            .content_type(&format!("multipart/form-data; boundary={boundary}"))
            .bytes(body.into())
            .await;
        assert_eq!(resp.status_code(), 400);
        assert!(!tmp.path().join(&batch_id).join("p.jpg").exists());
    }

    /// Scanner may send Content-Type: multipart/mixed or multipart/related instead of
    /// multipart/form-data. Axum's Multipart extractor rejects those — the server must
    /// normalise the type to form-data (preserving the boundary) before extraction.
    ///
    /// This test is RED until force_json is fixed to rewrite multipart/* → form-data.
    #[tokio::test]
    async fn test_image_upload_multipart_mixed_normalised_to_form_data() {
        let (server, _tmp) = test_server();
        let batch_id = create_batch(&server).await;
        let boundary = "mixedboundary456";
        let param = json!({"batch_id": batch_id}).to_string();
        let body = multipart_body(
            boundary,
            include_bytes!("../../tests/fixtures/scan_page.jpg"),
            "p.jpg",
            &param,
        );

        let resp = server
            .post("/NmWebService/image")
            .content_type(&format!("multipart/mixed; boundary={boundary}"))
            .bytes(body.into())
            .await;
        // Must succeed after fix — currently returns 400
        assert_eq!(resp.status_code(), 200);
    }

    #[tokio::test]
    async fn test_multipart_mixed_preserves_case_sensitive_boundary() {
        let (server, _tmp) = test_server();
        let batch_id = create_batch(&server).await;
        let boundary = "CaseSensitiveBOUNDARY";
        let param = json!({"batch_id": batch_id}).to_string();
        let body = multipart_body(
            boundary,
            include_bytes!("../../tests/fixtures/scan_page.jpg"),
            "p.jpg",
            &param,
        );
        let resp = server
            .post("/NmWebService/image")
            .content_type(&format!("multipart/mixed; boundary={boundary}"))
            .bytes(body.into())
            .await;
        assert_eq!(resp.status_code(), 200);
    }

    /// Images from the scanner are 2-4 MB each. Axum's default body limit is 2 MB,
    /// which would truncate the stream and cause "failed to read stream". The router
    /// must raise the limit to at least 100 MB.
    #[tokio::test]
    async fn test_image_upload_large_image_over_2mb() {
        let (server, _tmp) = test_server();
        let batch_id = create_batch(&server).await;
        let boundary = "largeboundary";
        let param = json!({"batch_id": batch_id}).to_string();
        // 3 MB of fake JPEG data — well above axum's 2 MB default limit
        let mut image = vec![0u8; 3 * 1024 * 1024];
        let fixture = include_bytes!("../../tests/fixtures/scan_page.jpg");
        image[..fixture.len()].copy_from_slice(fixture);
        let body = multipart_body(boundary, &image, "big.jpg", &param);

        let resp = server
            .post("/NmWebService/image")
            .content_type(&format!("multipart/form-data; boundary={boundary}"))
            .bytes(body.into())
            .await;
        assert_eq!(resp.status_code(), 200);
    }

    /// Path traversal via a crafted filename must return 400, not 200 or 500.
    /// This also tests that error detection is correct regardless of error message wording.
    #[tokio::test]
    async fn test_image_upload_path_traversal_rejected() {
        let (server, _tmp) = test_server();
        let batch_id = create_batch(&server).await;
        let boundary = "traversalboundary";
        let param = json!({"batch_id": batch_id}).to_string();
        let body = multipart_body(
            boundary,
            include_bytes!("../../tests/fixtures/scan_page.jpg"),
            "../escape.jpg",
            &param,
        );

        let resp = server
            .post("/NmWebService/image")
            .content_type(&format!("multipart/form-data; boundary={boundary}"))
            .bytes(body.into())
            .await;
        assert_eq!(resp.status_code(), 400);
    }

    /// Non-JPEG content (missing FF D8 FF magic bytes) must be rejected with 400.
    #[tokio::test]
    async fn test_image_upload_non_jpeg_rejected() {
        let (server, _tmp) = test_server();
        let batch_id = create_batch(&server).await;
        let boundary = "nonjpegboundary";
        let param = json!({"batch_id": batch_id}).to_string();
        // PNG magic bytes — definitely not a JPEG
        let body = multipart_body(boundary, b"\x89PNG\r\n\x1a\n", "page.jpg", &param);

        let resp = server
            .post("/NmWebService/image")
            .content_type(&format!("multipart/form-data; boundary={boundary}"))
            .bytes(body.into())
            .await;
        assert_eq!(resp.status_code(), 400);
    }

    #[tokio::test]
    async fn test_upload_returns_service_unavailable_when_all_slots_busy() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = AppState::new(Config {
            jobs: vec![Job {
                output_path: tmp.path().to_path_buf(),
                consume_path: None,
                job_info: json!({"name":"test", "job_id":0}),
                scan_settings: json!({}),
            }],
            ..Default::default()
        });
        let _all_slots = state
            .upload_slots
            .clone()
            .acquire_many_owned(2)
            .await
            .unwrap();
        let server = TestApp::new(state);
        let resp = server
            .post("/NmWebService/image")
            .content_type("multipart/form-data; boundary=busy")
            .bytes(
                multipart_body(
                    "busy",
                    include_bytes!("../../tests/fixtures/scan_page.jpg"),
                    "page.jpg",
                    "{\"batch_id\":\"none\"}",
                )
                .into(),
            )
            .await;
        assert_eq!(resp.status_code(), 503);
    }

    /// Content-Type with uppercase Multipart/ prefix must not be clobbered by force_json.
    ///
    /// This test is RED until force_json uses a case-insensitive check.
    #[tokio::test]
    async fn test_image_upload_content_type_uppercase_multipart() {
        let (server, _tmp) = test_server();
        let batch_id = create_batch(&server).await;
        let boundary = "upperboundary789";
        let param = json!({"batch_id": batch_id}).to_string();
        let body = multipart_body(
            boundary,
            include_bytes!("../../tests/fixtures/scan_page.jpg"),
            "p.jpg",
            &param,
        );

        let resp = server
            .post("/NmWebService/image")
            .content_type(&format!("Multipart/form-data; boundary={boundary}"))
            .bytes(body.into())
            .await;
        // Must succeed after fix — currently force_json overwrites with application/json
        assert_eq!(resp.status_code(), 200);
    }
}
