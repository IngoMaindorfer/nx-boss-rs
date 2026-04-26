use axum::{
    Json,
    extract::{Multipart, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::batch::PathTraversalError;
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

async fn parse_multipart(mut multipart: Multipart) -> Result<ParsedImage, Response> {
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
                        warn!(error = ?e, "failed to read image field");
                        return Err(bad_request(e));
                    }
                }
            }
            Some("parameter") => match field.bytes().await {
                Ok(b) => parameter_bytes = Some(b.to_vec()),
                Err(e) => {
                    warn!(error = %e, "failed to read parameter field");
                    return Err(bad_request(e));
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
            warn!("post_image: missing image field");
            return Err(bad_request("missing image field"));
        }
    };
    let parameters: Value = match parameter_bytes {
        Some(b) => match serde_json::from_slice(&b) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "post_image: invalid parameter JSON");
                return Err(bad_request(format!("invalid parameter JSON: {e}")));
            }
        },
        None => {
            warn!("post_image: missing parameter field");
            return Err(bad_request("missing parameter field"));
        }
    };

    Ok(ParsedImage {
        filename,
        content,
        parameters,
    })
}

pub async fn post_image(State(state): State<AppState>, multipart: Multipart) -> impl IntoResponse {
    let parsed = match parse_multipart(multipart).await {
        Ok(p) => p,
        Err(r) => return r,
    };

    let batch_id = match parsed.parameters["batch_id"].as_str() {
        Some(id) => id.to_string(),
        None => {
            warn!("post_image: missing batch_id in parameter JSON");
            return bad_request("missing batch_id in parameter");
        }
    };

    if !parsed.content.starts_with(&[0xFF, 0xD8, 0xFF]) {
        warn!(batch_id = %batch_id, filename = %parsed.filename, "post_image: not a JPEG");
        return bad_request("not a JPEG image");
    }

    // Remove batch before I/O so the lock is not held during file writes.
    let batch = match lock!(state.batches).remove(&batch_id) {
        Some(b) => b,
        None => {
            warn!(batch_id = %batch_id, "post_image: unknown batch_id");
            return StatusCode::NOT_FOUND.into_response();
        }
    };

    let filename = parsed.filename.clone();
    let content = parsed.content.clone();
    let parameters = parsed.parameters.clone();
    // add_file does file I/O — run off the async thread.
    let (batch, result) = tokio::task::spawn_blocking(move || {
        let mut batch = batch;
        let r = batch.add_file(&filename, &content, parameters);
        (batch, r)
    })
    .await
    .expect("spawn_blocking panicked");

    // Always re-insert — even on error, so the scanner can retry the page.
    lock!(state.batches).insert(batch_id.clone(), batch);

    match result {
        Ok(()) => {
            info!(
                batch_id = %batch_id,
                filename = %parsed.filename,
                bytes = parsed.content.len(),
                "image saved"
            );
            StatusCode::OK.into_response()
        }
        Err(e) if e.downcast_ref::<PathTraversalError>().is_some() => {
            warn!(batch_id = %batch_id, filename = %parsed.filename, "post_image: path traversal rejected");
            bad_request("bad filename")
        }
        Err(e) => {
            warn!(batch_id = %batch_id, error = %e, "post_image: failed to save image");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use axum_test::TestServer;
    use serde_json::{Value, json};

    use crate::config::{Config, Job};
    use crate::routes::router;
    use crate::state::AppState;

    fn test_server() -> (TestServer, tempfile::TempDir) {
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
        let server = TestServer::new(router(AppState::new(config)));
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

    async fn create_batch(server: &TestServer) -> String {
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
        let body = multipart_body(boundary, b"\xff\xd8\xff\xd9", "p.jpg", &param);

        let resp = server
            .post("/NmWebService/image")
            .content_type(&format!("multipart/form-data; boundary={boundary}"))
            .bytes(body.into())
            .await;
        assert_eq!(resp.status_code(), 200);
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
        let body = multipart_body(boundary, b"\xff\xd8\xff\xd9", "p.jpg", &param);

        let resp = server
            .post("/NmWebService/image")
            .content_type(&format!("multipart/mixed; boundary={boundary}"))
            .bytes(body.into())
            .await;
        // Must succeed after fix — currently returns 400
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
        image[..3].copy_from_slice(&[0xFF, 0xD8, 0xFF]);
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
        let body = multipart_body(boundary, b"\xff\xd8\xff\xd9", "../escape.jpg", &param);

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

    /// Content-Type with uppercase Multipart/ prefix must not be clobbered by force_json.
    ///
    /// This test is RED until force_json uses a case-insensitive check.
    #[tokio::test]
    async fn test_image_upload_content_type_uppercase_multipart() {
        let (server, _tmp) = test_server();
        let batch_id = create_batch(&server).await;
        let boundary = "upperboundary789";
        let param = json!({"batch_id": batch_id}).to_string();
        let body = multipart_body(boundary, b"\xff\xd8\xff\xd9", "p.jpg", &param);

        let resp = server
            .post("/NmWebService/image")
            .content_type(&format!("Multipart/form-data; boundary={boundary}"))
            .bytes(body.into())
            .await;
        // Must succeed after fix — currently force_json overwrites with application/json
        assert_eq!(resp.status_code(), 200);
    }
}
