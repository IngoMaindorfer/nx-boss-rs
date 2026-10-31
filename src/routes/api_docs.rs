use askama::Template;
use axum::{
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};

use super::{UiState, ui::render};
use crate::{build_info::BuildInfo, translations::Translations};

const OPENAPI: &str = include_str!("../../docs/openapi.json");

#[derive(Template)]
#[template(path = "api_docs.html")]
struct ApiDocsTpl {
    t: &'static Translations,
    build: &'static BuildInfo,
}

pub async fn page(UiState(state): UiState) -> Response {
    render(ApiDocsTpl {
        t: state.translations,
        build: state.build_info,
    })
}

pub async fn openapi_json() -> impl IntoResponse {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json; charset=utf-8"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-store"),
            ),
        ],
        OPENAPI,
    )
}

pub async fn script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../../static/api-docs.js"),
    )
}

#[cfg(test)]
mod tests {
    use axum_test::TestServer;
    use serde_json::Value;

    use crate::{config::Config, routes, state::AppState};

    const SCANNER_ROUTES: &[(&str, &str)] = &[
        ("/health", "get"),
        ("/NmWebService/heartbeat", "get"),
        ("/NmWebService/device", "post"),
        ("/NmWebService/authorization", "get"),
        ("/NmWebService/authorization", "post"),
        ("/NmWebService/scansetting", "get"),
        ("/NmWebService/batch", "post"),
        ("/NmWebService/batch/{batch_id}", "put"),
        ("/NmWebService/image", "post"),
        ("/NmWebService/accesstoken", "delete"),
    ];

    #[test]
    fn openapi_covers_scanner_router_with_auth_and_request_shapes() {
        let raw =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/openapi.json"))
                .expect("OpenAPI file exists");
        let spec: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(spec["openapi"], "3.1.0");
        assert_eq!(spec["info"]["version"], env!("CARGO_PKG_VERSION"));
        for &(path, method) in SCANNER_ROUTES {
            assert!(
                spec["paths"][path][method].is_object(),
                "missing {method} {path}"
            );
            assert!(spec["paths"][path][method]["responses"].is_object());
        }
        assert_eq!(
            spec["paths"]["/NmWebService/image"]["post"]["requestBody"]["content"]["multipart/form-data"]
                ["schema"]["required"],
            serde_json::json!(["image", "parameter"])
        );
        assert_eq!(
            spec["paths"]["/NmWebService/authorization"]["post"]["requestBody"]["content"]["application/json"]
                ["schema"]["$ref"],
            "#/components/schemas/ScannerLogin"
        );
        assert_eq!(
            spec["components"]["schemas"]["ScannerLogin"]["properties"]["auth_info_pass"]["writeOnly"],
            true
        );
        assert_eq!(
            spec["components"]["securitySchemes"]["scannerBearer"]["scheme"],
            "bearer"
        );
        for &(path, method) in SCANNER_ROUTES {
            if matches!(
                path,
                "/NmWebService/batch"
                    | "/NmWebService/image"
                    | "/NmWebService/scansetting"
                    | "/NmWebService/accesstoken"
            ) {
                assert!(spec["paths"][path][method]["security"].is_array());
            }
        }
    }

    #[tokio::test]
    async fn documentation_is_admin_only_and_serves_machine_readable_spec() {
        let state = AppState::new(Config::default());
        let scanner = TestServer::new(routes::scanner_router(state.clone()));
        assert_eq!(scanner.get("/api/openapi.json").await.status_code(), 404);
        assert_eq!(scanner.get("/api-docs").await.status_code(), 404);

        let admin = TestServer::new(routes::admin_router(state));
        let response = admin.get("/api/openapi.json").await;
        assert_ne!(
            response.status_code(),
            200,
            "anonymous access must be denied"
        );

        let merged = TestServer::new(routes::router(AppState::new(Config::default())));
        let response = merged.get("/api/openapi.json").await;
        assert_eq!(response.status_code(), 200);
        assert_eq!(response.json::<Value>()["openapi"], "3.1.0");
        let response = merged.get("/api-docs").await;
        assert_eq!(response.status_code(), 200);
        assert!(response.text().contains("/api/openapi.json"));
    }
}
