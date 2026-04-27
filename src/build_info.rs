pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_SHA: &str = match option_env!("GIT_SHA") {
    Some(s) => s,
    None => "dev",
};
pub const REPOSITORY: &str = "https://github.com/IngoMaindorfer/nx-boss-rs";

pub struct BuildInfo {
    pub version: &'static str,
    pub git_sha: &'static str,
    pub repository: &'static str,
}

impl BuildInfo {
    pub fn short_sha(&self) -> &str {
        let len = self.git_sha.len().min(7);
        &self.git_sha[..len]
    }

    pub fn commit_url(&self) -> String {
        if self.git_sha == "dev" {
            self.repository.to_string()
        } else {
            format!("{}/commit/{}", self.repository, self.git_sha)
        }
    }
}

pub static BUILD: BuildInfo = BuildInfo {
    version: VERSION,
    git_sha: GIT_SHA,
    repository: REPOSITORY,
};
