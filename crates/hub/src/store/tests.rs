use std::collections::BTreeSet;

use sqlx::Row;

use super::{testing::*, *};

#[test]
fn a_configured_url_is_postgres_or_an_absolute_sqlite_file_and_never_carries_a_password() {
    for good in [
        "postgres://hub@db.internal:5432/devagotchi",
        "postgresql://hub@10.40.0.30/devagotchi?sslmode=require",
        "postgres:///devagotchi?host=/run/postgresql",
        "sqlite:///var/lib/devagotchi-hub/state/accounts.sqlite3",
    ] {
        assert_eq!(check_configured_url(good), Ok(()), "{good}");
    }
    for bad in [
        "postgres://hub:hunter2@db.internal/devagotchi",
        "postgresql://hub:x@10.40.0.30/devagotchi",
        "sqlite://relative/accounts.sqlite3",
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
    let state = Path::new("/var/lib/devagotchi-hub/state");
    assert_eq!(
        url_for(None, state),
        "sqlite:///var/lib/devagotchi-hub/state/accounts.sqlite3?mode=rwc"
    );
    assert_eq!(
        url_for(Some("postgres://hub@db/devagotchi"), state),
        "postgres://hub@db/devagotchi"
    );
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
