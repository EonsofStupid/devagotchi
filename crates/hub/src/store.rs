//! The hub's store: Postgres for a hosted hub, SQLite built into the binary for a standalone
//! one (decision of 1 Nov: one storage layer, every test on both). One code path serves
//! both: SQL that both accept, `$1`-style parameters, times as Unix seconds and flags as 0
//! or 1 in 64-bit integers, bytes as BLOB or BYTEA. Each backend has its own migrations in
//! `migrations/<backend>/`, kept in step: same tables, columns and meaning.
//!
//! Every write goes through [`transaction!`]: one transaction that sees nothing another
//! writer does halfway. SQLite takes its write lock up front (`BEGIN IMMEDIATE`); Postgres
//! runs it SERIALIZABLE and runs it again when the server says it collided with another
//! one, so code written for one writer at a time stays right with many hub instances.
//!
//! Where the store is: `DEVAGOTCHI_HUB_STORAGE_URL` when set (how a Postgres password
//! arrives, from the secret store, never from a file), else the config's `storage.url`, else
//! `accounts.sqlite3` in the state directory.

use std::{
    path::{Path, PathBuf},
    sync::Once,
};

use devagotchi_contracts::{BridgeError, BridgeResult};
use sqlx::{
    Any, AnyPool, Executor, Transaction,
    any::{AnyPoolOptions, install_default_drivers},
    migrate::Migrator,
};

static SQLITE: Migrator = sqlx::migrate!("migrations/sqlite");
static POSTGRES: Migrator = sqlx::migrate!("migrations/postgres");

/// The environment variable that names the store, over the config's `storage.url`.
pub const STORAGE_URL_ENV: &str = "DEVAGOTCHI_HUB_STORAGE_URL";

/// How many times a Postgres transaction that collided with another is run again.
const RETRIES: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Sqlite,
    Postgres,
}

#[derive(Clone)]
pub struct Store {
    pool: AnyPool,
    backend: Backend,
}

impl std::fmt::Debug for Store {
    /// Never prints the URL: it may carry a password.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Store({:?})", self.backend)
    }
}

/// Why a transaction's work stopped: the store failed (and may be run again), or the work
/// refused (a `BridgeError` for the caller, nothing written).
pub enum Failed {
    Store(sqlx::Error),
    Refused(BridgeError),
}

impl From<sqlx::Error> for Failed {
    fn from(error: sqlx::Error) -> Self {
        Failed::Store(error)
    }
}

impl From<BridgeError> for Failed {
    fn from(error: BridgeError) -> Self {
        Failed::Refused(error)
    }
}

/// Run a block as one write transaction of `store` and commit it:
/// `transaction!(&store, |connection| { … })`, where the block is async code returning
/// `Result<T, Failed>` with `connection: &mut AnyConnection`. A refusal rolls everything
/// back and comes out as the `BridgeError`; on Postgres, a block that collided with
/// another transaction runs again from the start, so it must not act outside the store.
/// The value is a `BridgeResult<T>`. (A macro rather than a method taking an async
/// closure: the block stays in its caller, so whether the caller's future is `Send` is
/// worked out there, which a closure over a borrowed connection defeats.)
macro_rules! transaction {
    ($store:expr, |$connection:ident| $body:block) => {{
        let store: &$crate::store::Store = $store;
        let mut tries = 0u32;
        loop {
            let mut transaction = match store.begin().await {
                Ok(transaction) => transaction,
                Err(error) => break Err(error),
            };
            let outcome: Result<_, $crate::store::Failed> = async {
                let $connection: &mut ::sqlx::AnyConnection = &mut transaction;
                $body
            }
            .await;
            if let Some(done) = store.finish(transaction, outcome, &mut tries).await {
                break done;
            }
        }
    }};
}
pub(crate) use transaction;

pub fn store_error(error: sqlx::Error) -> BridgeError {
    BridgeError::new("account_store", error.to_string())
}

/// A time for the store: Unix seconds fit an i64 for the next 292 billion years.
pub fn secs(time: u64) -> i64 {
    i64::try_from(time).unwrap_or(i64::MAX)
}

/// A time from the store; a negative one (never written) reads as 0.
pub fn unsecs(time: i64) -> u64 {
    u64::try_from(time).unwrap_or(0)
}

/// The store a hub uses: the environment's URL, the config's, or the state directory's
/// SQLite file.
pub fn url_for(configured: Option<&str>, state_dir: &Path) -> String {
    match std::env::var(STORAGE_URL_ENV) {
        Ok(url) if !url.trim().is_empty() => url.trim().to_string(),
        _ => configured
            .map(str::to_string)
            .unwrap_or_else(|| sqlite_url(&state_dir.join("accounts.sqlite3"))),
    }
}

/// The URL of a SQLite store at `path`, in the one form sqlx reads back the same path on
/// every OS: `sqlite:` and the path with `/` separators (`sqlite:/var/lib/…`,
/// `sqlite:C:/ProgramData/…`), its `%`, `?`, `#` and spaces percent-encoded.
pub fn sqlite_url(path: &Path) -> String {
    let path = path.to_string_lossy();
    #[cfg(windows)]
    let path = path.replace('\\', "/");
    let mut encoded = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '%' => encoded.push_str("%25"),
            '?' => encoded.push_str("%3F"),
            '#' => encoded.push_str("%23"),
            ' ' => encoded.push_str("%20"),
            c => encoded.push(c),
        }
    }
    format!("sqlite:{encoded}?mode=rwc")
}

/// The file a SQLite URL names (`sqlite:PATH` or `sqlite://PATH`, before any `?`,
/// percent-decoded), or `None` for any other URL.
pub fn sqlite_path(url: &str) -> Option<PathBuf> {
    let rest = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))?;
    let path = rest.split('?').next().unwrap_or("");
    let decoded = percent_encoding::percent_decode_str(path).decode_utf8_lossy();
    Some(PathBuf::from(decoded.into_owned()))
}

/// Whether a URL is one the store opens, and keeps its password out of a file: `postgres://`
/// or `postgresql://` without a password, or `sqlite:` with an absolute path.
pub fn check_configured_url(url: &str) -> Result<(), String> {
    if let Some(rest) = url
        .strip_prefix("postgres://")
        .or_else(|| url.strip_prefix("postgresql://"))
    {
        let authority = rest.split(['/', '?']).next().unwrap_or("");
        if authority
            .rsplit_once('@')
            .is_some_and(|(user, _)| user.contains(':'))
        {
            return Err(format!(
                "storage.url must not carry a password; set {STORAGE_URL_ENV} from the secret store instead"
            ));
        }
        return Ok(());
    }
    if let Some(path) = sqlite_path(url) {
        return if path.is_absolute() {
            Ok(())
        } else {
            Err("storage.url sqlite: needs an absolute path".to_string())
        };
    }
    Err("storage.url is postgres://… or sqlite:/absolute/path".to_string())
}

impl Store {
    /// Open the store at `url` and bring its schema up to date. A SQLite file is the hub's
    /// alone (0600, with its -wal and -shm files), whether made here or found.
    pub async fn open(url: &str) -> BridgeResult<Self> {
        static DRIVERS: Once = Once::new();
        DRIVERS.call_once(install_default_drivers);
        let (backend, file) = if url.starts_with("postgres://") || url.starts_with("postgresql://")
        {
            (Backend::Postgres, None)
        } else if let Some(path) = sqlite_path(url) {
            (Backend::Sqlite, Some(path))
        } else {
            return Err(BridgeError::new(
                "account_store",
                "the store's URL is postgres://… or sqlite:…",
            ));
        };
        if let Some(file) = &file {
            if !file.exists() {
                // SQLite gives its -wal and -shm files the main file's mode: all three 0600.
                crate::push::write_private(file, &[])?;
            }
            make_private(file)?;
        }
        let pool = AnyPoolOptions::new()
            .max_connections(match backend {
                Backend::Sqlite => 4,
                Backend::Postgres => 16,
            })
            .after_connect(move |connection, _| {
                Box::pin(async move {
                    if backend == Backend::Sqlite {
                        connection
                            .execute(
                                "PRAGMA journal_mode = WAL;
                                 PRAGMA synchronous = FULL;
                                 PRAGMA busy_timeout = 5000;
                                 PRAGMA foreign_keys = ON;",
                            )
                            .await?;
                    }
                    Ok(())
                })
            })
            .connect(url)
            .await
            .map_err(store_error)?;
        let migrator = match backend {
            Backend::Sqlite => &SQLITE,
            Backend::Postgres => &POSTGRES,
        };
        migrator
            .run(&pool)
            .await
            .map_err(|error| BridgeError::new("account_store", error.to_string()))?;
        if let Some(file) = &file {
            make_private(file)?;
        }
        Ok(Self { pool, backend })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// A connection for reads outside a transaction.
    pub async fn read(&self) -> BridgeResult<sqlx::pool::PoolConnection<Any>> {
        self.pool.acquire().await.map_err(store_error)
    }

    /// A write transaction for [`transaction!`]: SQLite takes its write lock now; Postgres
    /// runs it SERIALIZABLE.
    pub async fn begin(&self) -> BridgeResult<Transaction<'static, Any>> {
        let begin = match self.backend {
            Backend::Sqlite => "BEGIN IMMEDIATE",
            Backend::Postgres => "BEGIN ISOLATION LEVEL SERIALIZABLE",
        };
        self.pool.begin_with(begin).await.map_err(store_error)
    }

    /// End a [`transaction!`]: commit what the work did, or roll back what it refused.
    /// `None` when Postgres found it collided with another transaction and it is to run
    /// again (counted in `tries`).
    pub async fn finish<T>(
        &self,
        transaction: Transaction<'static, Any>,
        outcome: Result<T, Failed>,
        tries: &mut u32,
    ) -> Option<BridgeResult<T>> {
        let outcome = match outcome {
            Ok(value) => transaction.commit().await.map(|()| value).map_err(Failed::Store),
            Err(failed) => {
                let _ = transaction.rollback().await;
                Err(failed)
            }
        };
        match outcome {
            Ok(value) => Some(Ok(value)),
            Err(Failed::Refused(error)) => Some(Err(error)),
            Err(Failed::Store(error)) if collided(&error) && *tries < RETRIES => {
                *tries += 1;
                None
            }
            Err(Failed::Store(error)) => Some(Err(store_error(error))),
        }
    }

    /// The pool, for tests that reach under the API.
    #[cfg(test)]
    pub fn pool(&self) -> &AnyPool {
        &self.pool
    }
}

/// Postgres refused a transaction because another one ran alongside it: serialization
/// failure (40001) or deadlock (40P01). Running it again is the remedy.
fn collided(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|e| e.code())
        .is_some_and(|code| code == "40001" || code == "40P01")
}

/// A SQLite store and its -wal and -shm files, when present, readable by the hub alone.
pub fn make_private(path: &Path) -> BridgeResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let name = path.as_os_str().to_owned();
        for suffix in ["", "-wal", "-shm"] {
            let mut file = name.clone();
            file.push(suffix);
            match std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(BridgeError::new(
                        "account_store",
                        format!("{}: {error}", Path::new(&file).display()),
                    ));
                }
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Stores for a test: a SQLite file always, and a fresh Postgres database when
/// `DEVAGOTCHI_TEST_POSTGRES_URL` names a server (CI sets it; every hub test that takes
/// these runs on both backends there).
#[cfg(test)]
pub mod testing {
    use super::*;

    pub const POSTGRES_ENV: &str = "DEVAGOTCHI_TEST_POSTGRES_URL";

    /// A store under test, with what keeps it alive and how to open it again.
    pub struct TestStore {
        pub store: Store,
        pub url: String,
        pub dir: tempfile::TempDir,
    }

    pub async fn stores() -> Vec<TestStore> {
        let mut out = vec![sqlite().await];
        if let Some(store) = postgres().await {
            out.push(store);
        }
        out
    }

    pub async fn sqlite() -> TestStore {
        let dir = tempfile::tempdir().unwrap();
        let url = sqlite_url(&dir.path().join("accounts.sqlite3"));
        TestStore {
            store: Store::open(&url).await.unwrap(),
            url,
            dir,
        }
    }

    /// A new, empty database on the test server, or `None` without one.
    pub async fn postgres() -> Option<TestStore> {
        let server = std::env::var(POSTGRES_ENV).ok().filter(|u| !u.is_empty())?;
        static DRIVERS: Once = Once::new();
        DRIVERS.call_once(install_default_drivers);
        let name = format!("dvg_test_{}", hex::encode(crate::link::random::<8>()));
        let admin = AnyPoolOptions::new()
            .max_connections(1)
            .connect(&server)
            .await
            .expect("the test Postgres server");
        admin
            .execute(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .await
            .unwrap();
        admin.close().await;
        let base = server.split('?').next().unwrap();
        let (root, _) = base.rsplit_once('/').unwrap();
        let url = format!("{root}/{name}");
        Some(TestStore {
            store: Store::open(&url).await.unwrap(),
            url,
            dir: tempfile::tempdir().unwrap(),
        })
    }
}

#[cfg(test)]
mod tests;
