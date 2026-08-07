use crate::model::Widget;

pub fn to_json(w: &Widget) -> String {
    serde_json::to_string(w).unwrap_or_else(|_| "{}".to_string())
}

pub fn from_json(s: &str) -> Option<Widget> {
    serde_json::from_str(s).ok()
}
