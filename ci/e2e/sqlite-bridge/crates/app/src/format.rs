use dbcore::Widget;

pub fn line(w: &Widget) -> String {
    format!("#{:>3}  {:<12} x{}", w.id, w.name, w.quantity)
}

pub fn header() -> String {
    format!("{:>4}  {:<12} {}", "id", "name", "qty")
}
