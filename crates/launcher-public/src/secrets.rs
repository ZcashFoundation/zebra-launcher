//! Provider tokens: in the OS keychain (`keyring`, service `org.zfnd.zeal`, account
//! `dns:<provider>:<zone>`), never in `launcher.json`, a log line or a status. The keychain is
//! reached off the async runtime. A 0600 file per account is the other store, for tests and,
//! later, an explicit choice without a Secret Service.

use std::{future::Future, path::PathBuf, pin::Pin};

use launcher_herd::public::names::KEYCHAIN_SERVICE;

/// Keeping or reading a token failed. The message never holds the token.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("the keychain is not available: {0}")]
    Unavailable(String),
    #[error("no token is stored")]
    Missing,
    #[error("the token store failed: {0}")]
    Io(String),
}

type Answer<T> = Pin<Box<dyn Future<Output = Result<T, SecretError>> + Send>>;

/// Where tokens are kept.
pub trait SecretStore: Send + Sync + 'static {
    fn set(&self, account: String, token: String) -> Answer<()>;
    fn get(&self, account: String) -> Answer<String>;
    fn delete(&self, account: String) -> Answer<()>;
}

/// The OS keychain: the Secret Service on Linux.
#[derive(Clone, Copy, Debug, Default)]
pub struct Keychain;

fn keychain_error(err: keyring::Error) -> SecretError {
    match err {
        keyring::Error::NoEntry => SecretError::Missing,
        // The error's own text names the store, never the secret.
        other => SecretError::Unavailable(other.to_string()),
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, SecretError> + Send + 'static,
) -> Result<T, SecretError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|err| SecretError::Io(err.to_string()))?
}

impl SecretStore for Keychain {
    fn set(&self, account: String, token: String) -> Answer<()> {
        Box::pin(blocking(move || {
            keyring::Entry::new(KEYCHAIN_SERVICE, &account)
                .and_then(|entry| entry.set_password(&token))
                .map_err(keychain_error)
        }))
    }

    fn get(&self, account: String) -> Answer<String> {
        Box::pin(blocking(move || {
            keyring::Entry::new(KEYCHAIN_SERVICE, &account)
                .and_then(|entry| entry.get_password())
                .map_err(keychain_error)
        }))
    }

    fn delete(&self, account: String) -> Answer<()> {
        Box::pin(blocking(move || {
            match keyring::Entry::new(KEYCHAIN_SERVICE, &account)
                .and_then(|entry| entry.delete_credential())
            {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(err) => Err(keychain_error(err)),
            }
        }))
    }
}

/// One owner-only file per account in a directory.
#[derive(Clone, Debug)]
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        FileStore { dir: dir.into() }
    }

    fn path(&self, account: &str) -> PathBuf {
        let safe: String = account
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.dir.join(safe)
    }
}

impl SecretStore for FileStore {
    fn set(&self, account: String, token: String) -> Answer<()> {
        let path = self.path(&account);
        Box::pin(blocking(move || {
            crate::store::write_private(&path, token.as_bytes())
                .map_err(|err| SecretError::Io(err.to_string()))
        }))
    }

    fn get(&self, account: String) -> Answer<String> {
        let path = self.path(&account);
        Box::pin(blocking(move || match std::fs::read_to_string(&path) {
            Ok(token) => Ok(token),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(SecretError::Missing),
            Err(err) => Err(SecretError::Io(err.to_string())),
        }))
    }

    fn delete(&self, account: String) -> Answer<()> {
        let path = self.path(&account);
        Box::pin(blocking(move || match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(SecretError::Io(err.to_string())),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_file_store_keeps_tokens_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path());
        let account = "dns:duckdns:zec-alice.duckdns.org".to_string();
        assert!(matches!(
            store.get(account.clone()).await,
            Err(SecretError::Missing)
        ));
        store
            .set(
                account.clone(),
                "a7c4d0ad-114e-40ef-ba1d-d217904a50f2".into(),
            )
            .await
            .unwrap();
        assert_eq!(
            store.get(account.clone()).await.unwrap(),
            "a7c4d0ad-114e-40ef-ba1d-d217904a50f2"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path(&account))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        store.delete(account.clone()).await.unwrap();
        store.delete(account.clone()).await.unwrap();
        assert!(matches!(
            store.get(account).await,
            Err(SecretError::Missing)
        ));
    }
}
