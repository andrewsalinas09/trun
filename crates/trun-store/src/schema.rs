//! Schema migrations, tracked with `PRAGMA user_version`.

use anyhow::Result;
use rusqlite::Connection;

const HUB_MIGRATIONS: &[&str] = &[
    // v1
    "CREATE TABLE projects (
        name       TEXT PRIMARY KEY,
        file       TEXT NOT NULL,
        created_at INTEGER NOT NULL
     ) STRICT;
     CREATE TABLE runs (
        id         TEXT PRIMARY KEY,
        project    TEXT NOT NULL,
        name       TEXT NOT NULL,
        host       TEXT NOT NULL,
        lifecycle  TEXT NOT NULL,
        health     TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        ended_at   INTEGER,
        summary    TEXT NOT NULL
     ) STRICT;
     CREATE INDEX runs_created   ON runs(created_at DESC);
     CREATE INDEX runs_lifecycle ON runs(lifecycle);
     CREATE INDEX runs_name      ON runs(name, created_at DESC);
     CREATE INDEX runs_project   ON runs(project, created_at DESC);",
    // v2: remote hosts
    "CREATE TABLE hosts (
        name       TEXT PRIMARY KEY,
        config     TEXT NOT NULL,
        created_at INTEGER NOT NULL
     ) STRICT;",
];

const PROJECT_MIGRATIONS: &[&str] = &[
    // v1
    "CREATE TABLE runs (
        id      TEXT PRIMARY KEY,
        summary TEXT NOT NULL
     ) STRICT;
     CREATE TABLE output (
        run_id TEXT NOT NULL,
        seq    INTEGER NOT NULL,
        ts     INTEGER NOT NULL,
        stream INTEGER NOT NULL,
        cr     INTEGER NOT NULL,
        text   TEXT NOT NULL,
        PRIMARY KEY (run_id, seq)
     ) STRICT, WITHOUT ROWID;
     CREATE TABLE events (
        run_id  TEXT NOT NULL,
        seq     INTEGER NOT NULL,
        ts      INTEGER NOT NULL,
        kind    TEXT NOT NULL,
        payload TEXT NOT NULL,
        PRIMARY KEY (run_id, seq)
     ) STRICT, WITHOUT ROWID;",
    // v2: metric points. SQLite turns NaN into NULL, so non-finite values are kept
    // in `special` (1 = NaN, 2 = +inf, 3 = -inf) with `value` NULL.
    "CREATE TABLE metrics (
        run_id  TEXT NOT NULL,
        name    TEXT NOT NULL,
        seq     INTEGER NOT NULL,
        ts      INTEGER NOT NULL,
        step    INTEGER,
        value   REAL,
        special INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (run_id, name, seq)
     ) STRICT, WITHOUT ROWID;",
];

pub fn migrate_hub(conn: &Connection) -> Result<()> {
    migrate(conn, HUB_MIGRATIONS)
}

pub fn migrate_project(conn: &Connection) -> Result<()> {
    migrate(conn, PROJECT_MIGRATIONS)
}

fn migrate(conn: &Connection, steps: &[&str]) -> Result<()> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    for (i, sql) in steps.iter().enumerate().skip(current as usize) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}
