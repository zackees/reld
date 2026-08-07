//! dbcore: a tiny SQLite-backed store used to exercise C linking (rusqlite bundled).
pub mod error;
pub mod model;
pub mod schema;
pub mod store;
pub mod query;
pub mod serialize;
pub mod util;

pub use error::DbError;
pub use model::Widget;
pub use store::Store;
