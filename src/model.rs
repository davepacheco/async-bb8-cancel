use diesel::prelude::*;

#[derive(Queryable, Selectable)]
#[diesel(table_name = crate::schema::counter)]
pub struct Counter {
    pub id: i32,
    pub count: i32,
}
