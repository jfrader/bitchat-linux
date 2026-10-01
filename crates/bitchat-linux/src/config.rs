use std::fs::{DirBuilder, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;

use crate::types::{APP_NAME, parse_geohash};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "BitChat Bluetooth mesh and internet chat in your terminal"
)]
pub struct Config {
    #[arg(long, alias = "name", value_parser = nickname)]
    pub nickname: Option<String>,
    #[arg(long, value_parser = channel)]
    pub geohash: Option<String>,
    #[arg(long, value_name = "URL", value_parser = relay)]
    pub relay: Vec<String>,
    #[arg(long)]
    pub no_bluetooth: bool,
    #[arg(long, value_name = "PATH")]
    pub data_dir: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub state_dir: Option<PathBuf>,
}

pub struct Paths {
    pub data: PathBuf,
    pub state: PathBuf,
}

impl Paths {
    pub fn prepare(&self) -> Result<()> {
        private_directory(&self.data)
    }
}

pub(crate) fn private_directory(path: &std::path::Path) -> Result<()> {
    match DirBuilder::new().recursive(true).mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error).with_context(|| format!("cannot create {}", path.display()));
        }
    }
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("cannot open private directory {}", path.display()))?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        bail!(
            "{} must be a private directory owned by this user (mode 0700)",
            path.display()
        );
    }
    Ok(())
}

impl Config {
    pub fn paths(&self) -> Result<Paths> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?;
        let xdg = |name: &str, fallback: &str| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| home.join(fallback))
                .join(APP_NAME)
        };
        let absolute = |path: PathBuf| -> Result<PathBuf> {
            if path.is_absolute() {
                Ok(path)
            } else {
                Ok(std::env::current_dir()?.join(path))
            }
        };
        Ok(Paths {
            data: absolute(
                self.data_dir
                    .clone()
                    .unwrap_or_else(|| xdg("XDG_DATA_HOME", ".local/share")),
            )?,
            state: absolute(
                self.state_dir
                    .clone()
                    .unwrap_or_else(|| xdg("XDG_STATE_HOME", ".local/state")),
            )?,
        })
    }
}

fn nickname(input: &str) -> Result<String, String> {
    bitchatd::sanitize_nickname(input)
        .ok_or_else(|| "nickname must contain visible characters".into())
}

fn channel(input: &str) -> Result<String, String> {
    parse_geohash(input).map_err(|error| error.to_string())
}

fn relay(input: &str) -> Result<String, String> {
    nostr_sdk::prelude::RelayUrl::parse(input)
        .map(|url| url.to_string())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn location_is_an_explicit_choice() {
        let config = Config::try_parse_from([APP_NAME]).unwrap();
        assert!(config.geohash.is_none());
        assert!(config.relay.is_empty());
    }

    #[test]
    fn rejects_invalid_channel_and_relay_before_starting_networking() {
        assert!(Config::try_parse_from([APP_NAME, "--geohash", "invalid"]).is_err());
        assert!(Config::try_parse_from([APP_NAME, "--relay", "https://example.com"]).is_err());
        assert!(Config::try_parse_from([APP_NAME, "--relay", "ws://127.0.0.1:8080"]).is_ok());
    }

    #[test]
    fn fresh_data_root_is_private_before_either_transport_starts() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            data: temp.path().join("missing/app"),
            state: temp.path().join("state"),
        };
        paths.prepare().unwrap();
        assert_eq!(fs::metadata(&paths.data).unwrap().mode() & 0o777, 0o700);
        fs::set_permissions(&paths.data, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(paths.prepare().is_err());
        let link = temp.path().join("link");
        symlink(&paths.data, &link).unwrap();
        assert!(private_directory(&link).is_err());
    }
}
