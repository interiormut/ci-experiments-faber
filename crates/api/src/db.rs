use diesel_async::{
    AsyncPgConnection,
    pooled_connection::{AsyncDieselConnectionManager, bb8::Pool},
};

pub type DbPool = Pool<AsyncPgConnection>;

pub async fn init_pool(database_url: &str) -> DbPool {
    let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(database_url);
    Pool::builder()
        .build(manager)
        .await
        .expect("failed to build DB connection pool")
}
