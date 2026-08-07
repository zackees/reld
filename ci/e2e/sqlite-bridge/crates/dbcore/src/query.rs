use crate::error::DbError;
use crate::model::Widget;
use rusqlite::{params, Connection, OptionalExtension};

pub fn insert_widget(conn: &Connection, w: &Widget) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO widgets (id, name, quantity) VALUES (?1, ?2, ?3)",
        params![w.id, w.name, w.quantity],
    )?;
    Ok(())
}

pub fn get_widget(conn: &Connection, id: i64) -> Result<Widget, DbError> {
    let row = conn
        .query_row(
            "SELECT id, name, quantity FROM widgets WHERE id = ?1",
            params![id],
            |r| {
                Ok(Widget {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    quantity: r.get(2)?,
                })
            },
        )
        .optional()?;
    row.ok_or(DbError::NotFound(id))
}

pub fn count_widgets(conn: &Connection) -> Result<i64, DbError> {
    let n = conn.query_row("SELECT COUNT(*) FROM widgets", [], |r| r.get(0))?;
    Ok(n)
}

pub fn sum_quantity(conn: &Connection) -> Result<i64, DbError> {
    let n = conn.query_row("SELECT COALESCE(SUM(quantity), 0) FROM widgets", [], |r| {
        r.get(0)
    })?;
    Ok(n)
}
