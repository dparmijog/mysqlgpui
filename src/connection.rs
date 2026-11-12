//! Connection parameters captured by the "new connection" form, saved
//! connections, and the live session that results from dialing one.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use sqldr_core::{ConnConfig, Driver};

use crate::config::{self, ConnEntry};

/// What the user typed into the "new connection" form.
#[derive(Debug, Clone)]
pub struct ConnectionParams {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
}

impl ConnectionParams {
    /// URL without the password (that one goes to the keyring). Built via
    /// `url::Url` so reserved characters in credentials are
    /// percent-encoded instead of corrupting the authority section.
    fn base_url(&self) -> Result<url::Url> {
        let host = self.host.trim();
        if host.is_empty() {
            bail!("host is required");
        }
        if self.user.is_empty() {
            bail!("user is required");
        }
        let mut url = url::Url::parse(&format!("mysql://{host}"))
            .with_context(|| format!("invalid host '{host}'"))?;
        if url.host_str().is_none() || !url.path().is_empty() {
            bail!("invalid host '{host}'");
        }
        let invalid = || anyhow::anyhow!("invalid credentials for host '{host}'");
        url.set_port(Some(self.port)).map_err(|_| invalid())?;
        url.set_username(&self.user).map_err(|_| invalid())?;
        Ok(url)
    }

    fn conn_config(&self) -> Result<ConnConfig> {
        let mut url = self.base_url()?;
        if !self.password.is_empty() {
            url.set_password(Some(&self.password))
                .map_err(|_| anyhow::anyhow!("invalid password"))?;
        }
        Ok(ConnConfig { name: self.name.clone(), url: url.into(), read_only: false })
    }
}

/// An established connection plus the databases it can see.
pub struct Session {
    pub driver: Arc<dyn Driver>,
    pub databases: Vec<String>,
}

async fn open(cfg: &ConnConfig) -> Result<Session> {
    let driver = sqldr_core::connect(cfg).await?;
    let databases = driver.list_databases().await?;
    Ok(Session { driver, databases })
}

/// Dials a saved connection (password from the keyring). Must run on the
/// tokio runtime.
pub async fn open_saved(entry: ConnEntry) -> Result<Session> {
    let url = config::resolve_url(&entry)?;
    open(&ConnConfig { name: entry.name, url, read_only: entry.read_only }).await
}

/// Tests the credentials against the real server and, only if they work,
/// persists the connection (config.toml + keyring). Must run on the tokio
/// runtime.
pub async fn create(params: ConnectionParams) -> Result<(ConnEntry, Session)> {
    let name = params.name.trim();
    if name.is_empty() {
        bail!("name is required");
    }
    if config::load()?.connections.iter().any(|c| c.name == name) {
        bail!("a connection named '{name}' already exists");
    }

    let session = open(&params.conn_config()?).await?;

    let entry =
        ConnEntry {
            name: name.to_string(),
            url: params.base_url()?.into(),
            read_only: false,
            rest: toml::Table::new(),
        };
    // Re-read right before writing so a concurrent sqldr edit isn't lost.
    let mut cfg = config::load()?;
    cfg.connections.push(entry.clone());
    config::save(&cfg)?;
    if !params.password.is_empty() {
        config::set_password(&entry.name, &params.password)
            .context("connection saved, but the password could not be stored in the keyring")?;
    }
    Ok((entry, session))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(host: &str, password: &str) -> ConnectionParams {
        ConnectionParams {
            name: "x".into(),
            host: host.into(),
            port: 3307,
            user: "app".into(),
            password: password.into(),
        }
    }

    #[test]
    fn reserved_characters_in_the_password_survive_the_url_round_trip() {
        let cfg = params("db.local", "p@ss/w:rd#1").conn_config().unwrap();
        let url = url::Url::parse(&cfg.url).unwrap();
        assert_eq!(url.host_str(), Some("db.local"));
        assert_eq!(url.port(), Some(3307));
        assert_eq!(url.username(), "app");
        let decoded = url::form_urlencoded::parse(format!("x={}", url.password().unwrap()).as_bytes())
            .next()
            .unwrap()
            .1
            .into_owned();
        assert_eq!(decoded, "p@ss/w:rd#1");
    }

    #[test]
    fn the_saved_url_never_contains_the_password() {
        let url = params("db.local", "secret").base_url().unwrap();
        assert_eq!(url.password(), None);
    }

    #[test]
    fn a_host_carrying_a_path_is_rejected() {
        assert!(params("db.local/evil", "").conn_config().is_err());
    }

    /// Real-server check, opt-in: `cargo test -- --ignored`. Uses
    /// MYSQLGPUI_TEST_URL (e.g. mysql://user:pw@127.0.0.1:3306).
    #[test]
    #[ignore]
    fn live_server_lists_databases_and_tables_and_rejects_a_bad_password() {
        let url = std::env::var("MYSQLGPUI_TEST_URL").expect("set MYSQLGPUI_TEST_URL");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ok = ConnConfig { name: "t".into(), url: url.clone(), read_only: false };
            let session = open(&ok).await.expect("valid credentials must connect");
            assert!(!session.databases.is_empty());
            let db = &session.databases[0];
            session.driver.tables(db).await.expect("listing tables of a visible database");

            let mut bad = url::Url::parse(&url).unwrap();
            bad.set_password(Some("definitely-wrong")).unwrap();
            let bad = ConnConfig { name: "t".into(), url: bad.into(), read_only: false };
            let err = match open(&bad).await {
                Ok(_) => panic!("a wrong password must not connect"),
                Err(err) => err,
            };
            assert!(
                matches!(err.downcast_ref(), Some(sqldr_core::DriverError::AuthFailed(_))),
                "unexpected error: {err:#}"
            );
        });
    }
}
