use crate::error::CoreError;
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn write<T: Serialize>(path: &Path, value: &T) -> Result<(), CoreError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|source| CoreError::io(Some("receipt"), Some(parent.to_path_buf()), source))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(
        ".{}-container-init-{stamp}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("receipt"),
        std::process::id()
    ));
    let encoded = serde_json::to_vec_pretty(value).map_err(|source| CoreError::Serialization {
        operation: "serialize receipt".to_owned(),
        source,
    })?;
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&encoded)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if let Err(source) = result {
        let _ = fs::remove_file(&temporary);
        return Err(CoreError::io(
            Some("receipt"),
            Some(path.to_path_buf()),
            source,
        ));
    }
    Ok(())
}
