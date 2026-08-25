use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::model::Artifact;

pub fn persist(artifacts: &[Artifact]) -> Result<usize> {
    let mut connection =
        Connection::open_in_memory().context("open in-memory artifact database")?;
    connection.execute_batch(
        "CREATE TABLE artifact (
            id TEXT PRIMARY KEY,
            path TEXT NOT NULL,
            kind TEXT NOT NULL,
            byte_count INTEGER NOT NULL,
            labels TEXT NOT NULL
        );",
    )?;
    let transaction = connection.transaction()?;
    {
        let mut insert = transaction.prepare(
            "INSERT INTO artifact (id, path, kind, byte_count, labels) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for artifact in artifacts {
            insert.execute(params![
                artifact.id.to_string(),
                artifact.path,
                artifact.kind,
                artifact.bytes.len() as i64,
                serde_json::to_string(&artifact.labels)?,
            ])?;
        }
    }
    transaction.commit()?;
    connection
        .query_row("SELECT COUNT(*) FROM artifact", [], |row| {
            row.get::<_, usize>(0)
        })
        .context("count persisted artifacts")
}
