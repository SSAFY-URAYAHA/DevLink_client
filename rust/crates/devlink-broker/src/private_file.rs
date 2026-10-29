//! Bounded private snapshots. Credentials never appear in errors.
use crate::credentials;
use anyhow::{Context, Result, ensure};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

pub fn read<T: DeserializeOwned>(path: &Path, limit: usize) -> Result<Option<T>> {
    read_bytes(path, limit)?
        .map(|bytes| {
            serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("Invalid private snapshot"))
        })
        .transpose()
}

pub fn read_bytes(path: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => anyhow::bail!("Cannot inspect private snapshot"),
        Ok(_) => {}
    }
    credentials::verify_path_safety(path)?;
    credentials::verify_private_permissions(path)?;
    let file = fs::File::open(path).context("Cannot open private snapshot")?;
    ensure!(
        file.metadata()?.is_file(),
        "Private snapshot is not a regular file"
    );
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .context("Cannot read private snapshot")?;
    ensure!(bytes.len() <= limit, "Private snapshot exceeds size limit");
    Ok(Some(bytes))
}

pub fn write<T: Serialize>(path: &Path, value: &T, limit: usize) -> Result<()> {
    let parent = path.parent().context("Private snapshot has no parent")?;
    credentials::ensure_safe_parent_directory(parent)?;
    if fs::symlink_metadata(path).is_ok() {
        credentials::verify_path_safety(path)?;
        credentials::verify_private_permissions(path)?;
    }
    credentials::set_private_permissions(parent)?;
    let bytes =
        serde_json::to_vec(value).map_err(|_| anyhow::anyhow!("Cannot encode private snapshot"))?;
    ensure!(bytes.len() <= limit, "Private snapshot exceeds size limit");
    let mut temp = tempfile::Builder::new()
        .prefix(".private-")
        .tempfile_in(parent)
        .context("Cannot prepare private snapshot")?;
    credentials::set_private_permissions(temp.path())?;
    temp.write_all(&bytes)
        .context("Cannot write private snapshot")?;
    temp.as_file()
        .sync_all()
        .context("Cannot flush private snapshot")?;
    credentials::verify_private_permissions(temp.path())?;
    temp.persist(path)
        .map_err(|_| anyhow::anyhow!("Cannot replace private snapshot"))?;
    credentials::verify_path_safety(path)?;
    credentials::verify_private_permissions(path)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
