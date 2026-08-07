use dbcore::{Store, Widget};

pub fn seed(store: &Store) -> Result<(), dbcore::DbError> {
    let widgets = [
        Widget::new(1, "sprocket", 10),
        Widget::new(2, "gear", 20),
        Widget::new(3, "flange", 30),
    ];
    for w in &widgets {
        store.insert(w)?;
    }
    Ok(())
}
