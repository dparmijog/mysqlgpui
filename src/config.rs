//! Saved connections, shared with the sqldr TUI: same
//! `~/.config/sqldr/config.toml` and same keyring service, so a connection
//! added in either app shows up in both.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const KEYRING_SERVICE: &str = "sqldr";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnEntry {
    pub name: String,
    /// Never carries the password; that lives in the keyring.
    pub url: String,
    #[serde(default)]
    pub read_only: bool,
    /// Per-connection keys this app doesn't know (see [`Config::rest`]).
    #[serde(flatten)]
    pub rest: toml::Table,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub connections: Vec<ConnEntry>,
    /// mysqlgpui's light/dark choice. Its own key: sqldr's `theme` holds a
    /// TUI theme name and is left untouched.
    #[serde(default, rename = "gpui_appearance", skip_serializing_if = "Option::is_none")]
    pub appearance: Option<crate::appearance::Appearance>,
    /// Every other key (sqldr's `theme`, anything added later) is carried
    /// through untouched so saving from here never clobbers sqldr settings.
    #[serde(flatten)]
    pub rest: toml::Table,
}

/// `SQLDR_CONFIG_DIR` overrides the location, same as in sqldr.
pub fn config_path() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("SQLDR_CONFIG_DIR") {
        return Ok(PathBuf::from(dir).join("config.toml"));
    }
    let dirs = directories::ProjectDirs::from("", "", "sqldr")
        .context("could not determine config directory")?;
    Ok(dirs.config_dir().join("config.toml"))
}

pub fn load() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

pub fn save(config: &Config) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = toml::to_string_pretty(config).context("serializing config")?;
    std::fs::write(&path, raw).with_context(|| format!("writing {}", path.display()))
}

fn keyring_entry(conn_name: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(KEYRING_SERVICE, conn_name)?)
}

pub fn get_password(conn_name: &str) -> Result<Option<String>> {
    match keyring_entry(conn_name)?.get_password() {
        Ok(pw) => Ok(Some(pw)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn set_password(conn_name: &str, password: &str) -> Result<()> {
    keyring_entry(conn_name)?.set_password(password)?;
    Ok(())
}

/// Full connection URL: the stored one plus the keyring password, unless
/// the URL already carries one.
pub fn resolve_url(entry: &ConnEntry) -> Result<String> {
    let mut url = url::Url::parse(&entry.url)
        .with_context(|| format!("invalid connection url for '{}'", entry.name))?;
    if url.password().is_none() {
        if let Some(pw) = get_password(&entry.name)? {
            url.set_password(Some(&pw))
                .map_err(|_| anyhow::anyhow!("could not set password on url for '{}'", entry.name))?;
        }
    }
    Ok(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_keys_this_app_does_not_know_about() {
        let raw = r#"
theme = "Gruvbox Dark"
future_setting = 3

[[connections]]
name = "a"
url = "mysql://u@h:3306"
color = "red"
"#;
        let mut cfg: Config = toml::from_str(raw).unwrap();
        cfg.connections.push(ConnEntry {
            name: "b".into(),
            url: "mysql://v@h:3306".into(),
            read_only: false,
            rest: toml::Table::new(),
        });
        cfg.appearance = Some(crate::appearance::Appearance::Dark);
        let back: toml::Table = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert_eq!(back["theme"].as_str(), Some("Gruvbox Dark"));
        assert_eq!(back["gpui_appearance"].as_str(), Some("dark"));
        let reread: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert_eq!(reread.appearance, Some(crate::appearance::Appearance::Dark));
        assert_eq!(back["future_setting"].as_integer(), Some(3));
        let conns = back["connections"].as_array().unwrap();
        assert_eq!(conns.len(), 2);
        assert_eq!(conns[0]["color"].as_str(), Some("red"));
    }
}
