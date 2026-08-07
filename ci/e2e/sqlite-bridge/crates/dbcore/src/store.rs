use crate::error::DbError;
use crate::model::Widget;
use crate::query;
use crate::schema;
use rusqlite::Connection;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open_in_memory() -> Result<Self, DbError> {
        let conn = Connection::open_in_memory()?;
        for stmt in schema::all_statements() {
            conn.execute_batch(stmt)?;
        }
        Ok(Store { conn })
    }

    pub fn insert(&self, w: &Widget) -> Result<(), DbError> {
        query::insert_widget(&self.conn, w)
    }

    pub fn get(&self, id: i64) -> Result<Widget, DbError> {
        query::get_widget(&self.conn, id)
    }

    pub fn count(&self) -> Result<i64, DbError> {
        query::count_widgets(&self.conn)
    }

    pub fn total_quantity(&self) -> Result<i64, DbError> {
        query::sum_quantity(&self.conn)
    }
}
