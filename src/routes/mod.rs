use crate::state::AppState;
use axum::{
    Router,
    routing::{delete, get, post, put},
};

mod batch;
mod image;
mod scanner;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/NmWebService/heartbeat", get(scanner::heartbeat))
        .route("/NmWebService/device", post(scanner::device))
        .route(
            "/NmWebService/authorization",
            get(scanner::get_authorization),
        )
        .route(
            "/NmWebService/authorization",
            post(scanner::post_authorization),
        )
        .route("/NmWebService/scansetting", get(scanner::scansetting))
        .route("/NmWebService/batch", post(batch::post_batch))
        .route("/NmWebService/batch/:batch_id", put(batch::put_batch))
        .route("/NmWebService/image", post(image::post_image))
        .route(
            "/NmWebService/accesstoken",
            delete(scanner::delete_accesstoken),
        )
        .with_state(state)
}
