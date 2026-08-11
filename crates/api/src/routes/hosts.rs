//! Execution environments — see `internal-docs/host.md`.
//!
//! Two things this module deliberately does *not* expose:
//!
//! * **No "is it up" field, and no probe-now route.** Reachability is answered
//!   by the connection attempt, and faber has no reach-the-machine layer yet, so
//!   a live probe endpoint could only fabricate one. `last_probe` is named for
//!   what it is — the most recent *observation* — so a caller rendering it says
//!   "last reachable 3h ago" rather than showing a status light.
//! * **No container lifecycle.** Containers are created and destroyed by the
//!   user; faber registers, resolves, and execs. `DELETE` on a container
//!   unregisters the row and leaves the container alone.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use chrono::{DateTime, Utc};
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::host::{
        ExecMode, Host, HostContainer, HostProbe, NewHost, NewHostContainer, NewHostProbe,
        Transport, UpdateHost, UpdateHostContainer,
    },
    routes::{clamp_limit, deserialize_optional_field},
    schema::{host, host_container, host_probe},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/hosts", post(create).get(list))
        .route("/api/hosts/{id}", get(fetch).patch(update).delete(remove))
        .route(
            "/api/hosts/{id}/containers",
            post(create_container).get(list_containers),
        )
        .route("/api/hosts/{id}/probes", post(record_probe).get(list_probes))
        .route(
            "/api/host-containers/{id}",
            patch(update_container).delete(unregister_container),
        )
}

// ---------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateHostRequest {
    name: String,
    transport: Transport,
    exec_mode: ExecMode,
    ssh_address: Option<String>,
    ssh_key_ref: Option<String>,
    docker_endpoint: Option<String>,
}

#[derive(Deserialize)]
struct UpdateHostRequest {
    name: Option<String>,
    transport: Option<Transport>,
    exec_mode: Option<ExecMode>,
    /// `null` explicitly clears the field; omitting it leaves it unchanged.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    ssh_address: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    ssh_key_ref: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    docker_endpoint: Option<Option<String>>,
    /// Operator intent: `true` stamps `disabled_at`, `false` clears it. Never
    /// an observation — a host nobody can reach is still enabled.
    disabled: Option<bool>,
}

#[derive(Serialize)]
struct HostResponse {
    id: Uuid,
    name: String,
    transport: String,
    exec_mode: String,
    ssh_address: Option<String>,
    ssh_key_ref: Option<String>,
    docker_endpoint: Option<String>,
    created_at: DateTime<Utc>,
    disabled_at: Option<DateTime<Utc>>,
    /// Registrations that have not been unregistered, oldest first.
    containers: Vec<ContainerResponse>,
    /// The most recent observation, or `null` if this host has never been
    /// probed. Advisory: it describes a past attempt, not present reachability.
    last_probe: Option<ProbeResponse>,
}

#[derive(Serialize)]
struct ContainerResponse {
    id: Uuid,
    host_id: Uuid,
    container_ref: String,
    name: Option<String>,
    root_path: String,
    created_at: DateTime<Utc>,
    unregistered_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
struct ProbeResponse {
    id: Uuid,
    host_id: Uuid,
    container_id: Option<Uuid>,
    probed_at: DateTime<Utc>,
    ok: bool,
    error: Option<String>,
    os: Option<String>,
    arch: Option<String>,
    shell: Option<String>,
    tools: Option<Value>,
    root_path: Option<String>,
}

fn container_response(c: &HostContainer) -> ContainerResponse {
    ContainerResponse {
        id: c.id,
        host_id: c.host_id,
        container_ref: c.container_ref.clone(),
        name: c.name.clone(),
        root_path: c.root_path.clone(),
        created_at: c.created_at,
        unregistered_at: c.unregistered_at,
    }
}

fn probe_response(p: &HostProbe) -> ProbeResponse {
    ProbeResponse {
        id: p.id,
        host_id: p.host_id,
        container_id: p.container_id,
        probed_at: p.probed_at,
        ok: p.ok,
        error: p.error.clone(),
        os: p.os.clone(),
        arch: p.arch.clone(),
        shell: p.shell.clone(),
        tools: p.tools.clone(),
        root_path: p.root_path.clone(),
    }
}

fn host_response(
    h: &Host,
    containers: Vec<ContainerResponse>,
    last_probe: Option<ProbeResponse>,
) -> HostResponse {
    HostResponse {
        id: h.id,
        name: h.name.clone(),
        transport: h.transport.clone(),
        exec_mode: h.exec_mode.clone(),
        ssh_address: h.ssh_address.clone(),
        ssh_key_ref: h.ssh_key_ref.clone(),
        docker_endpoint: h.docker_endpoint.clone(),
        created_at: h.created_at,
        disabled_at: h.disabled_at,
        containers,
        last_probe,
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn validate_name(name: &str) -> Result<(), AppError> {
    if name.is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    if name.chars().count() > 100 {
        return Err(AppError::BadRequest(
            "name must be 100 characters or fewer".into(),
        ));
    }
    Ok(())
}

/// Mirrors the `host_transport_config` CHECK, so a mismatch comes back as a
/// message naming the field rather than as an opaque constraint violation.
fn validate_transport_config(
    transport: Transport,
    ssh_address: Option<&str>,
) -> Result<(), AppError> {
    match transport {
        Transport::Ssh if ssh_address.is_none_or(str::is_empty) => Err(AppError::BadRequest(
            "ssh_address is required when transport is 'ssh'".into(),
        )),
        Transport::Local if ssh_address.is_some_and(|a| !a.is_empty()) => Err(
            AppError::BadRequest("ssh_address is only valid when transport is 'ssh'".into()),
        ),
        _ => Ok(()),
    }
}

/// `Some("")` means the caller sent whitespace, which is not a value — it
/// collapses to `None` so an empty string never reaches a nullable column.
fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Loads a host the caller owns, or `NotFound`. Ownership is checked on the
/// host for every nested route: containers and probes carry no `user_id` of
/// their own, so the host is the only place authorization can happen.
async fn owned_host(
    conn: &mut diesel_async::AsyncPgConnection,
    user_id: Uuid,
    id: Uuid,
) -> Result<Host, AppError> {
    host::table
        .filter(host::id.eq(id))
        .filter(host::user_id.eq(user_id))
        .select(Host::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "hosts.load"))?
        .ok_or(AppError::NotFound)
}

/// Active registrations for a set of hosts, oldest first.
async fn containers_for(
    conn: &mut diesel_async::AsyncPgConnection,
    host_ids: &[Uuid],
) -> Result<Vec<HostContainer>, AppError> {
    if host_ids.is_empty() {
        return Ok(Vec::new());
    }
    host_container::table
        .filter(host_container::host_id.eq_any(host_ids))
        .filter(host_container::unregistered_at.is_null())
        .order(host_container::created_at.asc())
        .select(HostContainer::as_select())
        .load(conn)
        .await
        .map_err(|err| AppError::db(err, "hosts.containers"))
}

/// The newest probe per host, in one round trip. `DISTINCT ON` keeps this from
/// dragging the whole append-only log across the wire just to read its tail.
async fn latest_probes(
    conn: &mut diesel_async::AsyncPgConnection,
    host_ids: &[Uuid],
) -> Result<Vec<HostProbe>, AppError> {
    if host_ids.is_empty() {
        return Ok(Vec::new());
    }
    host_probe::table
        .filter(host_probe::host_id.eq_any(host_ids))
        .distinct_on(host_probe::host_id)
        .order((host_probe::host_id, host_probe::probed_at.desc()))
        .select(HostProbe::as_select())
        .load(conn)
        .await
        .map_err(|err| AppError::db(err, "hosts.latest_probes"))
}

/// The newest probe for one host, or `None` if it has never been probed.
async fn latest_probe(
    conn: &mut diesel_async::AsyncPgConnection,
    host_id: Uuid,
) -> Result<Option<HostProbe>, AppError> {
    Ok(latest_probes(conn, &[host_id]).await?.into_iter().next())
}

// ---------------------------------------------------------------------------
// Hosts
// ---------------------------------------------------------------------------

async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(input): Json<CreateHostRequest>,
) -> ApiResult<(StatusCode, Json<HostResponse>)> {
    let name = input.name.trim();
    validate_name(name)?;

    let ssh_address = trimmed(input.ssh_address.as_deref());
    validate_transport_config(input.transport, ssh_address)?;

    let new_host = NewHost {
        id: Uuid::now_v7(),
        user_id: user.id,
        name,
        transport: input.transport.as_str(),
        exec_mode: input.exec_mode.as_str(),
        ssh_address,
        ssh_key_ref: trimmed(input.ssh_key_ref.as_deref()),
        docker_endpoint: trimmed(input.docker_endpoint.as_deref()),
    };

    let mut conn = state.db.get().await?;

    let inserted: Host = diesel::insert_into(host::table)
        .values(&new_host)
        .returning(Host::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| match err {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => AppError::BadRequest(format!("a host named '{name}' already exists")),
            other => AppError::db(other, "hosts.create"),
        })?;

    // Freshly created: no registrations and nothing observed yet.
    Ok((
        StatusCode::CREATED,
        Json(host_response(&inserted, Vec::new(), None)),
    ))
}

async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<Vec<HostResponse>>> {
    let mut conn = state.db.get().await?;

    let hosts: Vec<Host> = host::table
        .filter(host::user_id.eq(user.id))
        .order(host::created_at.asc())
        .select(Host::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "hosts.list"))?;

    let ids: Vec<Uuid> = hosts.iter().map(|h| h.id).collect();
    let containers = containers_for(&mut conn, &ids).await?;
    let probes = latest_probes(&mut conn, &ids).await?;

    Ok(Json(
        hosts
            .iter()
            .map(|h| {
                host_response(
                    h,
                    containers
                        .iter()
                        .filter(|c| c.host_id == h.id)
                        .map(container_response)
                        .collect(),
                    probes
                        .iter()
                        .find(|p| p.host_id == h.id)
                        .map(probe_response),
                )
            })
            .collect(),
    ))
}

async fn fetch(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<HostResponse>> {
    let mut conn = state.db.get().await?;
    let found = owned_host(&mut conn, user.id, id).await?;

    let containers = containers_for(&mut conn, &[found.id]).await?;
    let probe = latest_probe(&mut conn, found.id).await?;

    Ok(Json(host_response(
        &found,
        containers.iter().map(container_response).collect(),
        probe.as_ref().map(probe_response),
    )))
}

async fn update(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateHostRequest>,
) -> ApiResult<Json<HostResponse>> {
    let mut conn = state.db.get().await?;
    let current = owned_host(&mut conn, user.id, id).await?;

    if let Some(name) = input.name.as_deref() {
        validate_name(name.trim())?;
    }

    // The sparse-column rule spans two fields, so it has to be checked against
    // the row the patch will produce, not against the patch alone: clearing
    // `ssh_address` is fine on a local host and a 400 on an ssh one.
    let effective_transport = match input.transport {
        Some(t) => t,
        None if current.transport == "ssh" => Transport::Ssh,
        None => Transport::Local,
    };
    let effective_ssh_address = match input.ssh_address {
        Some(ref value) => trimmed(value.as_deref()),
        None => current.ssh_address.as_deref(),
    };
    validate_transport_config(effective_transport, effective_ssh_address)?;

    // A transport switch that leaves the old address behind would fail the DB
    // check, so normalize: going local clears the address alongside it.
    let ssh_address_patch = match (input.transport, input.ssh_address.as_ref()) {
        (_, Some(value)) => Some(trimmed(value.as_deref())),
        (Some(Transport::Local), None) => Some(None),
        _ => None,
    };

    let name_trimmed = input.name.as_deref().map(str::trim);
    let patch = UpdateHost {
        name: name_trimmed,
        transport: input.transport.map(|t| t.as_str()),
        exec_mode: input.exec_mode.map(|m| m.as_str()),
        ssh_address: ssh_address_patch,
        ssh_key_ref: input.ssh_key_ref.as_ref().map(|v| trimmed(v.as_deref())),
        docker_endpoint: input.docker_endpoint.as_ref().map(|v| trimmed(v.as_deref())),
        disabled_at: input.disabled.map(|d| d.then(Utc::now)),
    };

    let updated: Host = diesel::update(
        host::table
            .filter(host::id.eq(id))
            .filter(host::user_id.eq(user.id)),
    )
    .set(patch)
    .returning(Host::as_returning())
    .get_result(&mut conn)
    .await
    .map_err(|err| match err {
        diesel::result::Error::NotFound => AppError::NotFound,
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => AppError::BadRequest("a host with that name already exists".into()),
        other => AppError::db(other, "hosts.update"),
    })?;

    let containers = containers_for(&mut conn, &[updated.id]).await?;
    let probe = latest_probe(&mut conn, updated.id).await?;

    Ok(Json(host_response(
        &updated,
        containers.iter().map(container_response).collect(),
        probe.as_ref().map(probe_response),
    )))
}

/// Drops the registration and, by cascade, its containers and probe history.
/// Nothing on the machine itself is touched. `PATCH { disabled: true }` is the
/// reversible alternative.
async fn remove(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;

    let deleted = diesel::delete(
        host::table
            .filter(host::id.eq(id))
            .filter(host::user_id.eq(user.id)),
    )
    .execute(&mut conn)
    .await
    .map_err(|err| AppError::db(err, "hosts.delete"))?;

    if deleted == 0 {
        return Err(AppError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Containers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateContainerRequest {
    container_ref: String,
    name: Option<String>,
    root_path: String,
}

#[derive(Deserialize)]
struct UpdateContainerRequest {
    container_ref: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    name: Option<Option<String>>,
    root_path: Option<String>,
    /// `false` re-registers a row that was unregistered earlier.
    unregistered: Option<bool>,
}

#[derive(Deserialize)]
struct ListContainersQuery {
    /// Unregistered rows are hidden by default — they are history, and a stale
    /// ref resolves no better than a missing one.
    #[serde(default)]
    include_unregistered: bool,
}

/// Registers a container faber should know about. It does not create one: under
/// R2 the user owns container lifecycle, and this row is an assertion that
/// faber knows the ref, not that the ref resolves.
async fn create_container(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(host_id): Path<Uuid>,
    Json(input): Json<CreateContainerRequest>,
) -> ApiResult<(StatusCode, Json<ContainerResponse>)> {
    let container_ref = input.container_ref.trim();
    if container_ref.is_empty() {
        return Err(AppError::BadRequest("container_ref is required".into()));
    }

    let root_path = input.root_path.trim();
    if root_path.is_empty() {
        return Err(AppError::BadRequest("root_path is required".into()));
    }
    // Path normalization is mandatory: bind-mounted and native paths both
    // present as `root_path`, and a relative one would make the agent learn
    // host-specific path habits that silently fail to transfer.
    if !root_path.starts_with('/') {
        return Err(AppError::BadRequest("root_path must be absolute".into()));
    }

    let mut conn = state.db.get().await?;
    let parent = owned_host(&mut conn, user.id, host_id).await?;

    if parent.exec_mode != ExecMode::Docker.as_str() {
        return Err(AppError::BadRequest(
            "containers can only be registered on a host whose exec_mode is 'docker'".into(),
        ));
    }

    let new_container = NewHostContainer {
        id: Uuid::now_v7(),
        host_id: parent.id,
        container_ref,
        name: trimmed(input.name.as_deref()),
        root_path,
    };

    let inserted: HostContainer = diesel::insert_into(host_container::table)
        .values(&new_container)
        .returning(HostContainer::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| match err {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => AppError::BadRequest(format!(
                "'{container_ref}' is already registered on this host"
            )),
            other => AppError::db(other, "hosts.containers.create"),
        })?;

    Ok((
        StatusCode::CREATED,
        Json(container_response(&inserted)),
    ))
}

async fn list_containers(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(host_id): Path<Uuid>,
    Query(query): Query<ListContainersQuery>,
) -> ApiResult<Json<Vec<ContainerResponse>>> {
    let mut conn = state.db.get().await?;
    let parent = owned_host(&mut conn, user.id, host_id).await?;

    let mut statement = host_container::table
        .filter(host_container::host_id.eq(parent.id))
        .into_boxed();

    if !query.include_unregistered {
        statement = statement.filter(host_container::unregistered_at.is_null());
    }

    let rows: Vec<HostContainer> = statement
        .order(host_container::created_at.asc())
        .select(HostContainer::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "hosts.containers.list"))?;

    Ok(Json(rows.iter().map(container_response).collect()))
}

/// Loads a container whose host the caller owns, or `NotFound`.
async fn owned_container(
    conn: &mut diesel_async::AsyncPgConnection,
    user_id: Uuid,
    id: Uuid,
) -> Result<HostContainer, AppError> {
    let row: Option<HostContainer> = host_container::table
        .inner_join(host::table)
        .filter(host_container::id.eq(id))
        .filter(host::user_id.eq(user_id))
        .select(HostContainer::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "hosts.containers.load"))?;

    row.ok_or(AppError::NotFound)
}

async fn update_container(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateContainerRequest>,
) -> ApiResult<Json<ContainerResponse>> {
    if let Some(ref container_ref) = input.container_ref
        && container_ref.trim().is_empty()
    {
        return Err(AppError::BadRequest("container_ref cannot be empty".into()));
    }

    if let Some(ref root_path) = input.root_path {
        let root_path = root_path.trim();
        if root_path.is_empty() {
            return Err(AppError::BadRequest("root_path cannot be empty".into()));
        }
        if !root_path.starts_with('/') {
            return Err(AppError::BadRequest("root_path must be absolute".into()));
        }
    }

    let mut conn = state.db.get().await?;
    let current = owned_container(&mut conn, user.id, id).await?;

    let patch = UpdateHostContainer {
        container_ref: input.container_ref.as_deref().map(str::trim),
        name: input.name.as_ref().map(|v| trimmed(v.as_deref())),
        root_path: input.root_path.as_deref().map(str::trim),
        unregistered_at: input.unregistered.map(|u| u.then(Utc::now)),
    };

    let updated: HostContainer = diesel::update(
        host_container::table.filter(host_container::id.eq(current.id)),
    )
    .set(patch)
    .returning(HostContainer::as_returning())
    .get_result(&mut conn)
    .await
    .map_err(|err| match err {
        diesel::result::Error::NotFound => AppError::NotFound,
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => AppError::BadRequest("that ref is already registered on this host".into()),
        other => AppError::db(other, "hosts.containers.update"),
    })?;

    Ok(Json(container_response(&updated)))
}

/// Ends the registration. The container itself is untouched — faber never owned
/// it. `PATCH { unregistered: false }` brings the row back.
async fn unregister_container(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;
    let current = owned_container(&mut conn, user.id, id).await?;

    diesel::update(host_container::table.filter(host_container::id.eq(current.id)))
        .set(host_container::unregistered_at.eq(Some(Utc::now())))
        .execute(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "hosts.containers.unregister"))?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Probes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RecordProbeRequest {
    /// Scopes the observation to one registered container on this host.
    container_id: Option<Uuid>,
    ok: bool,
    error: Option<String>,
    os: Option<String>,
    arch: Option<String>,
    shell: Option<String>,
    /// Capability manifest, e.g. `{"git": "2.43.0"}`. Capability is discovered
    /// by probe, never assumed from the host's mode.
    tools: Option<Value>,
    root_path: Option<String>,
}

#[derive(Deserialize)]
struct ListProbesQuery {
    limit: Option<i64>,
}

/// Appends one observation. Write-only history: there is no route to amend or
/// delete a probe, because the log is what makes "last attempt: connection
/// refused" a fact rather than a cached status.
async fn record_probe(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(host_id): Path<Uuid>,
    Json(input): Json<RecordProbeRequest>,
) -> ApiResult<(StatusCode, Json<ProbeResponse>)> {
    if !input.ok && trimmed(input.error.as_deref()).is_none() {
        return Err(AppError::BadRequest(
            "error is required when ok is false".into(),
        ));
    }

    let mut conn = state.db.get().await?;
    let parent = owned_host(&mut conn, user.id, host_id).await?;

    if let Some(container_id) = input.container_id {
        let container = owned_container(&mut conn, user.id, container_id).await?;
        if container.host_id != parent.id {
            return Err(AppError::BadRequest(
                "container_id names a container on a different host".into(),
            ));
        }
    }

    let new_probe = NewHostProbe {
        id: Uuid::now_v7(),
        host_id: parent.id,
        container_id: input.container_id,
        ok: input.ok,
        error: trimmed(input.error.as_deref()),
        os: trimmed(input.os.as_deref()),
        arch: trimmed(input.arch.as_deref()),
        shell: trimmed(input.shell.as_deref()),
        tools: input.tools,
        root_path: trimmed(input.root_path.as_deref()),
    };

    let inserted: HostProbe = diesel::insert_into(host_probe::table)
        .values(&new_probe)
        .returning(HostProbe::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "hosts.probes.create"))?;

    Ok((StatusCode::CREATED, Json(probe_response(&inserted))))
}

async fn list_probes(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(host_id): Path<Uuid>,
    Query(query): Query<ListProbesQuery>,
) -> ApiResult<Json<Vec<ProbeResponse>>> {
    let mut conn = state.db.get().await?;
    let parent = owned_host(&mut conn, user.id, host_id).await?;

    let rows: Vec<HostProbe> = host_probe::table
        .filter(host_probe::host_id.eq(parent.id))
        .order(host_probe::probed_at.desc())
        .limit(clamp_limit(query.limit))
        .select(HostProbe::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "hosts.probes.list"))?;

    Ok(Json(rows.iter().map(probe_response).collect()))
}
