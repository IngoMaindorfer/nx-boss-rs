use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// Replace one file using a private, synced temporary file in the same directory.
pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .context("file path has no filename")?
        .to_string_lossy();
    let tmp = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp).context("creating temporary file")?;
        file.write_all(bytes).context("writing temporary file")?;
        file.sync_all().context("syncing temporary file")?;
        std::fs::rename(&tmp, path).context("replacing file")?;
        sync_dir(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

pub fn sync_dir(path: &Path) -> Result<()> {
    std::fs::File::open(path)
        .context("opening directory for sync")?
        .sync_all()
        .context("syncing directory")
}
