use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Widget {
    pub id: i64,
    pub name: String,
    pub quantity: i64,
}

impl Widget {
    pub fn new(id: i64, name: impl Into<String>, quantity: i64) -> Self {
        Widget {
            id,
            name: name.into(),
            quantity,
        }
    }
}
