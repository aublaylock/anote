// Imports
use crate::config;
use std::path::PathBuf;

pub(crate) fn data_dir() -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(config::DATADIR))
}

pub(crate) fn pkg_data_dir() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join(config::APP_NAME))
}

pub(crate) fn locale_dir() -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(config::LOCALEDIR))
}
