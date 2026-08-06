use diesel::prelude::*;

use crate::schema::blob;

#[allow(dead_code)]
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = blob)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Blob {
    pub digest: Vec<u8>,
    pub data: Option<Vec<u8>>,
    pub storage_path: Option<String>,
    pub byte_length: i64,
    pub refcount: i64,
    pub created_at: i64,
}

#[derive(Insertable)]
#[diesel(table_name = blob)]
pub struct NewBlob<'a> {
    pub digest: &'a [u8],
    pub data: Option<&'a [u8]>,
    pub storage_path: Option<&'a str>,
    pub byte_length: i64,
    pub created_at: i64,
}
