/// End-to-end test for the complete scanner session protocol:
/// heartbeat → device → GET auth → POST auth → scansetting →
/// POST batch → POST image → PUT batch → UI verification
///
/// Mirrors the flow from scripts/rev.py, but runs in-process via axum_test.
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
                    "name": "E2EJob", "job_id": 0, "color": "#4D4D4D",
                    "type": 0, "job_setting": {}, "hierarchy_list": null
                }),
                scan_settings: json!({}),
            }],
            ..Default::default()
        };
        (TestServer::new(router(AppState::new(config))), tmp)
    }

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

    #[tokio::test]
    async fn test_full_scanner_session() {
        let (server, tmp) = test_server();

        // 1. Heartbeat — scanner checks server is alive
        let resp = server.get("/NmWebService/heartbeat").await;
        assert_eq!(resp.status_code(), 200);
        assert!(resp.json::<Value>()["system_time"].is_string());

        // 2. Device registration — scanner announces itself
        let resp = server
            .post("/NmWebService/device")
            .json(&json!({
                "call_timing": "startup",
                "scanner_ip":  "10.0.0.99",
                "scanner_mac": "00:80:17:e7:6f:33",
                "scanner_model": "fi-7300NX",
                "scanner_name": "fi-7300NX-E2E",
                "scanner_port": "10447",
                "scanner_protocol": "http",
                "serial_no": "E2E001"
            }))
            .await;
        assert_eq!(resp.status_code(), 200);

        // Scanner should now appear online in the UI
        assert!(
            server
                .get("/api/scanner-status")
                .await
                .text()
                .contains("online")
        );

        // 3. GET authorization — negotiate auth type
        let resp = server
            .get("/NmWebService/authorization")
            .add_query_param("auth_token", "")
            .await;
        assert_eq!(resp.status_code(), 200);
        assert_eq!(resp.json::<Value>()["auth_type"], "none");

        // 4. POST authorization — get job list + access token
        let resp = server
            .post("/NmWebService/authorization")
            .json(&json!({
                "auth_type": "none",
                "scanner_info": {
                    "ip": "10.0.0.99", "mac": "00:80:17:e7:6f:33",
                    "model": "fi-7300NX", "name": "fi-7300NX-E2E",
                    "port": "10447", "protocol": "http", "serial_no": "E2E001"
                }
            }))
            .await;
        assert_eq!(resp.status_code(), 200);
        let body = resp.json::<Value>();
        let jobs = body["job_info"].as_array().expect("job_info must be array");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0]["name"], "E2EJob");
        let job_id = jobs[0]["job_id"].as_u64().unwrap();

        // 5. GET scansetting — scanner fetches scan parameters
        let resp = server
            .get("/NmWebService/scansetting")
            .add_query_param("job_id", job_id.to_string())
            .await;
        assert_eq!(resp.status_code(), 200);

        // 6. POST batch — start scan session
        let resp = server
            .post("/NmWebService/batch")
            .json(&json!({ "job_id": job_id }))
            .await;
        assert_eq!(resp.status_code(), 200);
        let batch_id = resp.json::<Value>()["batch_id"]
            .as_str()
            .unwrap()
            .to_string();

        // 7. POST image — upload two pages
        for (i, page) in [b"\xff\xd8\xff\xd9".as_ref(), b"\xff\xd8\xff\xe0".as_ref()]
            .iter()
            .enumerate()
        {
            let boundary = format!("boundary{i}");
            let param = json!({ "batch_id": batch_id }).to_string();
            let body = multipart_body(&boundary, page, &format!("page{i}.jpg"), &param);
            let resp = server
                .post("/NmWebService/image")
                .content_type(&format!("multipart/form-data; boundary={boundary}"))
                .bytes(body.into())
                .await;
            assert_eq!(resp.status_code(), 200, "image upload {i} failed");
        }

        // Files must exist on disk
        assert!(tmp.path().join(&batch_id).join("page0.jpg").exists());
        assert!(tmp.path().join(&batch_id).join("page1.jpg").exists());

        // 8. PUT batch — scanner signals end of document
        let resp = server.put(&format!("/NmWebService/batch/{batch_id}")).await;
        assert_eq!(resp.status_code(), 200);

        // Completed batch must show up in the UI scan list
        assert!(server.get("/scans").await.text().contains(&batch_id[..8]));

        // Batch detail page must render successfully
        assert_eq!(
            server
                .get(&format!("/scans/{batch_id}"))
                .await
                .status_code(),
            200
        );

        // metadata.json must record 2 pages and completed=true
        let meta: Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join(&batch_id).join("metadata.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(meta["completed"], true);
        assert_eq!(meta["files"].as_array().unwrap().len(), 2);
        assert_eq!(meta["scanner"]["serial"], "E2E001");

        // 9. DELETE accesstoken — scanner logout
        assert_eq!(
            server
                .delete("/NmWebService/accesstoken")
                .await
                .status_code(),
            200
        );
    }
}
