use diesel::{ExpressionMethods, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use uuid::Uuid;

use crate::{
    crypto::decrypt_key,
    error::{ApiResult, AppError},
    models::{credential::Credential, model_config::ModelConfig},
    schema::{credentials, models},
    state::AppState,
};

pub struct ResolvedModel {
    pub config: ModelConfig,
    pub api_key: String,
}

/// Resolves `alias` to a decrypted API key for `user_id`.
///
/// Returns `NotFound` if the model does not exist or belongs to a different user.
/// Returns `Internal` if decryption fails — a corrupt or replaced key is not surfaced in detail.
pub async fn resolve_model(
    state: &AppState,
    user_id: Uuid,
    alias: &str,
) -> ApiResult<ResolvedModel> {
    let mut conn = state.db.get().await?;

    let model: ModelConfig = models::table
        .filter(models::user_id.eq(user_id))
        .filter(models::alias.eq(alias))
        .select(ModelConfig::as_select())
        .first(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "resolve.model_by_alias"))?;

    let cred_id = model.credential_id.ok_or_else(|| {
        AppError::BadRequest("model has no credential attached".into())
    })?;

    let cred: Credential = credentials::table
        .filter(credentials::id.eq(cred_id))
        .filter(credentials::user_id.eq(user_id))
        .select(Credential::as_select())
        .first(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "resolve.credential_lookup"))?;

    let key_bytes = decrypt_key(
        &cred.key_ciphertext,
        &cred.key_nonce,
        state.master_key.as_bytes(),
        cred.id,
        user_id,
    )
    .map_err(|_| AppError::Internal)?;

    let api_key = String::from_utf8(key_bytes).map_err(|_| AppError::Internal)?;

    Ok(ResolvedModel { config: model, api_key })
}
