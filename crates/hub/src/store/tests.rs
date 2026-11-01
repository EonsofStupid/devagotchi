use std::collections::BTreeSet;

use sqlx::Row;

use super::{testing::*, *};

#[test]
fn a_configured_url_is_postgres_or_an_absolute_sqlite_file_and_never_carries_a_password() {
    let state = std::env::temp_dir().join("devagotchi hub#1").join("accounts.sqlite3");
    for good in [
        "postgres://hub@db.internal:5432/devagotchi".to_string(),
        "postgresql://hub@10.40.0.30/devagotchi?sslmode=require".to_string(),
        "postgres:///devagotchi?host=/run/postgresql".to_string(),
        sqlite_url(&state),
    ] {
        assert_eq!(check_configured_url(&good), Ok(()), "{good}");
    }
    for bad in [
        "postgres://hub:hunter2@db.internal/devagotchi",
        "postgresql://hub:x@10.40.0.30/devagotchi",
        "sqlite:relative/accounts.sqlite3",
        "mysql://hub@db/devagotchi",
        "",
    ] {
        let refused = check_configured_url(bad).unwrap_err();
        assert!(!refused.contains("hunter2"), "{refused}");
    }
}

#[test]
fn without_a_url_the_store_is_the_state_directorys_sqlite_file() {
    if std::env::var(STORAGE_URL_ENV).is_ok() {
        return; // the environment names the store; nothing else is consulted
    }
    let state = std::env::temp_dir().join("state");
    let url = url_for(None, &state);
    assert_eq!(sqlite_path(&url), Some(state.join("accounts.sqlite3")));
    assert_eq!(
        url_for(Some("postgres://hub@db/devagotchi"), &state),
        "postgres://hub@db/devagotchi"
    );
}

#[tokio::test]
async fn a_sqlite_path_with_spaces_and_url_characters_round_trips() {
    // Spaces, `#`, `%` (and `?` where a file name may hold one): the file opened is the
    // file named.
    let dir = tempfile::tempdir().unwrap();
    let name = if cfg!(windows) { "my state #2 100%" } else { "my state #2 ?100%" };
    let path = dir.path().join(name).join("accounts.sqlite3");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let url = sqlite_url(&path);
    assert_eq!(sqlite_path(&url), Some(path.clone()));
    Store::open(&url).await.unwrap();
    assert!(path.is_file(), "{url}");
}

#[tokio::test]
async fn a_store_never_prints_its_url() {
    for test in stores().await {
        let shown = format!("{:?}", test.store);
        assert!(!shown.contains("://"), "{shown}");
    }
}

#[tokio::test]
async fn writers_racing_on_one_row_each_count_once() {
    // Read, add one, write back: eight at once. SQLite lets one writer in at a time;
    // Postgres runs them side by side, refuses the ones that collided, and they run again.
    // Either way the count is eight.
    for test in stores().await {
        let store = test.store.clone();
        sqlx::raw_sql(
            "CREATE TABLE counter (id BIGINT PRIMARY KEY, n BIGINT NOT NULL);
             INSERT INTO counter (id, n) VALUES (1, 0);",
        )
        .execute(&store.pool)
        .await
        .unwrap();
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let store = store.clone();
            tasks.push(tokio::spawn(async move {
                transaction!(&store, |connection| {
                    let n: i64 = sqlx::query_scalar("SELECT n FROM counter WHERE id = 1")
                        .fetch_one(&mut *connection)
                        .await?;
                    tokio::task::yield_now().await;
                    sqlx::query("UPDATE counter SET n = $1 WHERE id = 1")
                        .bind(n + 1)
                        .execute(&mut *connection)
                        .await?;
                    Ok(())
                })
            }));
        }
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        let n: i64 = sqlx::query_scalar("SELECT n FROM counter WHERE id = 1")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(n, 8, "{:?}", store.backend());
    }
}

#[tokio::test]
async fn a_refusal_rolls_back_what_the_transaction_did() {
    for test in stores().await {
        let store = test.store;
        sqlx::raw_sql("CREATE TABLE notes (body TEXT NOT NULL)")
            .execute(&store.pool)
            .await
            .unwrap();
        let refused: BridgeResult<()> = transaction!(&store, |connection| {
            sqlx::query("INSERT INTO notes (body) VALUES ('written')")
                .execute(&mut *connection)
                .await?;
            Err(BridgeError::new("refused", "not this one").into())
        });
        assert_eq!(refused.unwrap_err().code, "refused");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notes")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}

#[tokio::test]
async fn both_backends_have_the_same_tables_and_columns() {
    // The two migration directories are kept in step by hand; this holds them to it. Runs
    // where the test Postgres server is set (CI).
    let Some(postgres) = postgres().await else {
        return;
    };
    let sqlite = sqlite().await;
    let mut lite = BTreeSet::new();
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE '\\_%' ESCAPE '\\' AND name NOT LIKE 'sqlite%'",
    )
    .fetch_all(&sqlite.store.pool)
    .await
    .unwrap();
    for table in tables {
        let rows = sqlx::query("SELECT name FROM pragma_table_info($1)")
            .bind(&table)
            .fetch_all(&sqlite.store.pool)
            .await
            .unwrap();
        for row in rows {
            lite.insert((table.clone(), row.get::<String, _>(0)));
        }
    }
    let rows = sqlx::query(
        "SELECT table_name::text, column_name::text FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name NOT LIKE '\\_%'",
    )
    .fetch_all(&postgres.store.pool)
    .await
    .unwrap();
    let pg: BTreeSet<(String, String)> = rows
        .iter()
        .map(|row| (row.get::<String, _>(0), row.get::<String, _>(1)))
        .collect();
    assert!(!lite.is_empty());
    assert_eq!(lite, pg);
}

#[test]
fn sqlx_reads_a_windows_path_back_from_its_url() {
    // What the hub does on Windows, checked on every OS: the URL goes through `Url` (the
    // `any` driver) and back to SQLite's options, and the drive-letter path survives.
    use std::str::FromStr;
    let url = "sqlite:C:/Users/runner/AppData/Local/Temp/my%20state/accounts.sqlite3?mode=rwc";
    let any = sqlx::any::AnyConnectOptions::from_str(url).unwrap();
    let sqlite = sqlx::sqlite::SqliteConnectOptions::try_from(&any).unwrap();
    assert_eq!(
        sqlite.get_filename(),
        Path::new("C:/Users/runner/AppData/Local/Temp/my state/accounts.sqlite3")
    );
}

#[test]
fn schema_names_need_no_quoting_and_are_never_postgress_own() {
    for good in ["devagotchi_hub", "hub", "t_0123"] {
        assert!(valid_schema(good), "{good}");
    }
    for bad in [
        "",
        "Hub",
        "pg_catalog",
        "9lives",
        "hub; drop table accounts",
        "hub\"",
        &"a".repeat(64),
    ] {
        assert!(!valid_schema(bad), "{bad:?}");
    }
}

#[tokio::test]
async fn on_postgres_the_hubs_tables_live_in_their_own_schema() {
    // A database something else also uses (a Supabase project): the hub's tables, its
    // migration record included, go in their own schema, made if missing; `public` stays
    // as it was.
    let Some(url) = empty_postgres().await else {
        return;
    };
    let store = Store::open_in(&url, Some("devagotchi_hub")).await.unwrap();
    let mut placed = std::collections::BTreeMap::<String, Vec<String>>::new();
    let rows = sqlx::query(
        "SELECT table_schema::text, table_name::text FROM information_schema.tables
         WHERE table_schema IN ('public', 'devagotchi_hub') ORDER BY 1, 2",
    )
    .fetch_all(&store.pool)
    .await
    .unwrap();
    for row in rows {
        placed
            .entry(row.get::<String, _>(0))
            .or_default()
            .push(row.get::<String, _>(1));
    }
    assert!(!placed.contains_key("public"), "{placed:?}");
    let hub = &placed["devagotchi_hub"];
    for table in ["accounts", "passkeys", "sessions", "oauth_tokens", "_sqlx_migrations"] {
        assert!(hub.iter().any(|t| t == table), "{table} in {hub:?}");
    }
    // Opened again, as a second hub instance would: the schema and its tables are found.
    let again = Store::open_in(&url, Some("devagotchi_hub")).await.unwrap();
    let accounts = crate::accounts::Accounts::open(again).await.unwrap();
    accounts
        .create("acct_aaaaaaaaaaaaaaaaaaaaaaaaaa", "Jesse", true)
        .await
        .unwrap();
    assert!(accounts.owner().await.unwrap().is_some());
    // A name that is not a plain schema name is refused before anything runs.
    assert_eq!(
        Store::open_in(&url, Some("hub; drop schema public"))
            .await
            .unwrap_err()
            .code,
        "account_store"
    );
}

#[tokio::test]
async fn sqlite_has_no_schemas_and_ignores_one() {
    let test = sqlite().await;
    let store = Store::open_in(&test.url, Some("devagotchi_hub")).await.unwrap();
    let accounts = crate::accounts::Accounts::open(store).await.unwrap();
    assert!(accounts.owner().await.unwrap().is_none());
}
