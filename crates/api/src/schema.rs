// @generated automatically by Diesel CLI.

diesel::table! {
    users (id) {
        id -> Uuid,
        identity_id -> Uuid,
        username -> Varchar,
        display_name -> Text,
        avatar_url -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
