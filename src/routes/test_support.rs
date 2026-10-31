//! Authorized fixtures that send requests to the actual, separate production routers.
use crate::{admin_auth::AdminAuth, state::AppState};
use axum_test::{TestRequest, TestServer};

pub struct TestApp {
    pub admin: TestServer,
    pub scanner: TestServer,
    cookie: String,
    _auth_directory: tempfile::TempDir,
}
impl TestApp {
    pub fn new(mut state: AppState) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let auth = state
            .admin_auth
            .clone()
            .unwrap_or_else(|| AdminAuth::open(directory.path(), &[0x42; 32]).unwrap());
        let session = auth.test_session();
        state.admin_auth = Some(auth);
        let mut admin = TestServer::new(super::admin_router_with_hosts(
            state.clone(),
            vec!["localhost".into(), "127.0.0.1".into()],
        ));
        admin.add_header("host", "localhost");
        let cookie = format!("__Host-nx-admin={session}");
        admin.add_header("cookie", cookie.clone());
        let scanner = TestServer::new(super::scanner_router(state));
        Self {
            admin,
            scanner,
            cookie,
            _auth_directory: directory,
        }
    }
    pub fn cookie(&self, extra: &str) -> String {
        format!("{}; {extra}", self.cookie)
    }
    fn listener(&self, path: &str) -> &TestServer {
        if path == "/health" || path.starts_with("/NmWebService/") {
            &self.scanner
        } else {
            &self.admin
        }
    }
    pub fn get(&self, path: &str) -> TestRequest {
        self.listener(path).get(path)
    }
    pub fn post(&self, path: &str) -> TestRequest {
        self.listener(path).post(path)
    }
    pub fn put(&self, path: &str) -> TestRequest {
        self.listener(path).put(path)
    }
    pub fn delete(&self, path: &str) -> TestRequest {
        self.listener(path).delete(path)
    }
}
