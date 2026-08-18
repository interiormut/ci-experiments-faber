//! Operating the machines faber provides — see `internal-docs/service-hosts.md`.
//!
//! Everything under `/api/admin` answers a question no user can ask: what
//! shared machines exist, who is on them, and how much of each one any single
//! tenant may take. The rest of the API is written so that a user *cannot*
//! reach these rows — every user-facing write filters `user_id = $me` and a
//! service host's owner is NULL, so "users cannot edit the shared host" falls
//! out of a filter rather than out of a rule. This module is the deliberate
//! exception, and it is gated by [`AdminUser`] on every route.
//!
//! Four things live here:
//!
//! * **Hosts.** Registering a machine faber operates, setting the per-user
//!   ceilings every tenant gets by default, and draining one.
//! * **Grants.** One user's ceiling on one host, replacing the default
//!   *wholesale* — a grant is the answer in full, never a patch over the
//!   defaults, which is the only thing that makes "grant this user unlimited"
//!   expressible.
//! * **Tenants.** Who is materialised on a machine, and releasing one — which
//!   destroys their directory on it. The user-facing half of that, for
//!   somebody dropping their *own* materialisation, is in `routes::hosts`.
//! * **Service images.** A service host runs faber's own templates and nothing
//!   else, so a shared host with no service image on it is a machine nobody
//!   can spawn on.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post, put},
};
use chrono::{DateTime, Utc};
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    auth::AdminUser,
    error::{ApiResult, AppError},
    models::host::{
        ExecMode, Host, Image, NewHost, NewHostUserQuota, Transport, UpdateHost, UpdateImage,
    },
    routes::deserialize_optional_field,
    schema::{host, host_user_quota, image, users},
    service_hosts,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/hosts", post(create_host).get(list_hosts))
        .route(
            "/api/admin/hosts/{id}",
            get(fetch_host).patch(update_host).delete(delete_host),
        )
        .route(
            "/api/admin/hosts/{id}/agent",
            post(enroll_agent).delete(revoke_agent),
        )
        .route("/api/admin/hosts/{id}/users", get(list_tenants))
        .route(
            "/api/admin/hosts/{id}/users/{user_id}",
            delete(release_tenant),
        )
        .route(
            "/api/admin/hosts/{id}/users/{user_id}/quota",
            put(grant_quota).delete(revoke_quota),
        )
        .route("/api/admin/images", post(create_image).get(list_images))
        .route(
            "/api/admin/images/{id}",
            axum::routing::patch(update_image).delete(delete_image),
        )
}

// ---------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------

/// Four limits, and `null` is unlimited in every one of them.
///
/// The same shape carries a host's defaults and one user's grant, because they
/// are the same four numbers meaning the same thing at two scopes. Nothing
/// here is ever merged with anything else: a grant replaces the defaults as a
/// unit, so a `null` in a grant means unlimited and never "fall back".
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
struct Limits {
    /// 1000 is one core. A hard ceiling, not a share — an idle machine does
    /// not lend anyone more.
    cpu_millis: Option<i32>,
    memory_bytes: Option<i64>,
    /// The one limit faber refuses to overcommit, so raising it is checked
    /// against what the filesystem actually holds.
    storage_bytes: Option<i64>,
    /// Registered containers, running or not: a stopped container still holds
    /// the directory its storage was reserved for.
    container_max: Option<i32>,
}

impl From<&Host> for Limits {
    fn from(host: &Host) -> Self {
        Limits {
            cpu_millis: host.default_cpu_millis,
            memory_bytes: host.default_memory_bytes,
            storage_bytes: host.default_storage_bytes,
            container_max: host.default_container_max,
        }
    }
}

#[derive(Deserialize)]
struct CreateHostRequest {
    name: String,
    /// The daemon faber talks to. Explicit, like every other local docker
    /// host: faber must never fall back to whatever docker context the
    /// process happens to have.
    docker_endpoint: String,
    /// Parent of the per-user directories. Each tenant gets one, carrying the
    /// project quota that is their storage limit, so this has to be on a
    /// filesystem mounted with project quotas on.
    user_data_root: String,
    /// What the daemon's `--userns-remap` maps container uid 0 to — the first
    /// subuid of its remap user, as `/etc/subuid` records it. Tenant
    /// directories are given to this uid, so a wrong number means every
    /// container is refused its own workspace.
    container_root_uid: u32,
    #[serde(default)]
    defaults: Limits,
}

#[derive(Deserialize)]
struct UpdateHostRequest {
    name: Option<String>,
    docker_endpoint: Option<String>,
    user_data_root: Option<String>,
    /// Changing this re-owns every tenant directory on the next launch, which
    /// is what makes it a correction rather than a migration: the daemon's
    /// mapping is the truth and this column is only a record of it.
    container_root_uid: Option<u32>,
    /// Replaces all four defaults at once. Omitted leaves them; sent, every
    /// field is the whole answer, including the `null`s.
    defaults: Option<Limits>,
    /// The drain marker. `true` stops new launches and leaves everything
    /// already running alone.
    disabled: Option<bool>,
}

#[derive(Serialize)]
struct ServiceHostResponse {
    id: Uuid,
    name: String,
    docker_endpoint: Option<String>,
    user_data_root: Option<String>,
    /// What faber believes the daemon's userns-remap maps container root to.
    /// Faber records it and never reads it off the machine, so an operator
    /// comparing this against `/etc/subuid` is the check that it is right.
    container_root_uid: Option<i64>,
    created_at: DateTime<Utc>,
    /// Set means draining: no new launches, existing containers untouched.
    disabled_at: Option<DateTime<Utc>>,
    defaults: Limits,
    /// Users materialised here — the ones holding a directory and a storage
    /// reservation, not everyone who could arrive.
    tenants: i64,
    /// What the filesystem says, read live and asked of the host's own
    /// daemon. `null` when faber cannot read it — most often because no
    /// daemon is connected, which `agent` below says plainly.
    storage: Option<StorageResponse>,
    agent: AgentResponse,
}

/// Whether the machine is actually reachable, and whether it ever was.
///
/// Read from the live connection registry rather than from a column, on the
/// same rule that keeps host liveness out of the schema: a connection that
/// died is simply gone from the registry, so there is no stale value to
/// serve. The two fields answer different questions — `enrolled_at` is null
/// until somebody runs the install command, and `connected` is false whenever
/// the daemon is not on the wire *right now*.
#[derive(Serialize)]
struct AgentResponse {
    connected: bool,
    enrolled_at: Option<DateTime<Utc>>,
}

/// The number an operator actually needs before making a grant: not "how full
/// is the disk" but "how much is already promised, and how much is left to
/// promise".
#[derive(Serialize)]
struct StorageResponse {
    total_bytes: i64,
    available_bytes: i64,
    /// Total capacity less the reserve faber keeps free, which is the most
    /// that may be promised at once.
    ceiling_bytes: i64,
    /// Promised to materialised tenants, whether or not they are using it.
    committed_bytes: i64,
}

#[derive(Serialize)]
struct TenantResponse {
    user_id: Uuid,
    /// Their stable 32-bit id: the project id carrying their storage quota,
    /// the uid their containers run as, and the name they appear under in the
    /// machine's own logs.
    subject_id: Option<i32>,
    materialised_at: DateTime<Utc>,
    /// Registrations they hold here, which is what `container_max` bounds.
    containers: i64,
    quota: ResolvedQuotaResponse,
    /// Read from the machine on every request and stored nowhere. `null`
    /// fields mean the machine did not answer, not that usage is zero.
    usage: UsageResponse,
}

#[derive(Serialize)]
struct ResolvedQuotaResponse {
    #[serde(flatten)]
    limits: Limits,
    /// Whether a grant supplied these rather than the host's defaults.
    granted: bool,
    /// When a temporary grant lapses. An expiry is honoured the instant it
    /// passes, so it can shrink a limit underneath work already running.
    expires_at: Option<DateTime<Utc>>,
    note: Option<String>,
}

#[derive(Serialize)]
struct UsageResponse {
    memory_bytes: Option<u64>,
    pids: Option<u64>,
    storage_bytes: Option<u64>,
}

/// A grant, sent whole.
///
/// Every field is the entire answer for that resource: omitting one grants
/// unlimited rather than leaving the default in place. That is why this is a
/// `PUT` — the row it writes replaces the defaults as a unit, and a `PATCH`
/// that merged field by field would turn `null` back into "inherit" and make
/// "grant this user unlimited storage" unsayable.
#[derive(Deserialize)]
struct GrantRequest {
    #[serde(flatten)]
    limits: Limits,
    /// Absent for a standing grant. A temporary one is revoked by the clock,
    /// which the read path honours immediately.
    expires_at: Option<DateTime<Utc>>,
    /// Why. Grants outlive the conversation that produced them.
    note: Option<String>,
}

#[derive(Deserialize)]
struct CreateImageRequest {
    name: String,
    reference: String,
    default_mounts: Option<Value>,
    default_root_path: String,
}

#[derive(Deserialize)]
struct UpdateImageRequest {
    name: Option<String>,
    reference: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    default_mounts: Option<Option<Value>>,
    default_root_path: Option<String>,
}

#[derive(Serialize)]
struct ImageResponse {
    id: Uuid,
    name: String,
    reference: String,
    default_mounts: Option<Value>,
    default_root_path: String,
    created_at: DateTime<Utc>,
}

fn image_response(i: &Image) -> ImageResponse {
    ImageResponse {
        id: i.id,
        name: i.name.clone(),
        reference: i.reference.clone(),
        default_mounts: i.default_mounts.clone(),
        default_root_path: i.default_root_path.clone(),
        created_at: i.created_at,
    }
}

// ---------------------------------------------------------------------------
// Validation and loading
// ---------------------------------------------------------------------------

fn validate_name(name: &str) -> ApiResult<()> {
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

/// A service host's endpoint names a socket on the *agent's* machine, so
/// `tcp://` has nothing to dial: the forward opens a `direct-streamlocal`
/// channel, which is a unix socket by definition. The old table CHECK said
/// the same thing for a different reason — it stood in for "the limits and
/// the containers are on one machine" — and that reason is now carried by
/// the connection itself. This one is only about what can be reached.
fn validate_service_endpoint(endpoint: &str) -> ApiResult<()> {
    if !endpoint.starts_with("unix://") && !endpoint.starts_with('/') {
        return Err(AppError::BadRequest(
            "docker_endpoint must name the daemon's unix socket on the host's own \
             filesystem, as 'unix:///var/run/docker.sock' or an absolute path"
                .into(),
        ));
    }
    Ok(())
}

/// The data root has to be absolute for the same reason every other root does,
/// and additionally because it is resolved on the machine rather than in this
/// process — a relative path would be read against whatever directory the API
/// happens to have been started in.
fn validate_data_root(root: &str) -> ApiResult<()> {
    if !root.starts_with('/') {
        return Err(AppError::BadRequest(
            "user_data_root must be absolute".into(),
        ));
    }
    Ok(())
}

/// Zero is the one value here that must be refused, because it is not a
/// mistyped uid — it is host root, and it is precisely the state
/// `--userns-remap` exists to leave. A host recorded with it would chown every
/// tenant tree to root and then hand containers a workspace they cannot write,
/// which is the silent failure the CHECK constraint was added to prevent and
/// would sail straight past it.
///
/// Nothing narrower is checked. A remap base is conventionally far above the
/// system range, but "conventionally" is not a rule faber gets to enforce
/// against somebody's `/etc/subuid`, and §7 of the runbook compares the two
/// directly.
fn validate_container_root_uid(uid: u32) -> ApiResult<()> {
    if uid == 0 {
        return Err(AppError::BadRequest(
            "container_root_uid must not be 0: that is host root, which means the daemon \
             has no userns-remap configured"
                .into(),
        ));
    }
    Ok(())
}

/// Limits are ceilings, and a ceiling of zero is a host nobody can use. A
/// negative one is a typo the database would take.
fn validate_limits(limits: &Limits) -> ApiResult<()> {
    let positive = |value: Option<i64>, field: &str| -> ApiResult<()> {
        match value {
            Some(value) if value <= 0 => Err(AppError::BadRequest(format!(
                "{field} must be greater than zero, or null for unlimited"
            ))),
            _ => Ok(()),
        }
    };
    positive(limits.cpu_millis.map(i64::from), "cpu_millis")?;
    positive(limits.memory_bytes, "memory_bytes")?;
    positive(limits.storage_bytes, "storage_bytes")?;
    positive(limits.container_max.map(i64::from), "container_max")?;
    Ok(())
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// Loads a host faber operates, or `NotFound`.
///
/// The `user_id IS NULL` filter is what keeps this surface to service hosts
/// alone: an administrator operates the machines faber provides, and someone
/// else's private host is not one of them. Being an administrator is not being
/// able to edit every user's registrations.
async fn service_host(conn: &mut AsyncPgConnection, id: Uuid) -> ApiResult<Host> {
    host::table
        .filter(host::id.eq(id))
        .filter(host::user_id.is_null())
        .select(Host::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "admin.hosts.load"))?
        .ok_or(AppError::NotFound)
}

/// What the filesystem holds and what has already been promised out of it.
///
/// Best effort, and now for a second reason as well as the first: the read is
/// a `statvfs` on the host, asked of the daemon installed there, so a host
/// nobody has enrolled yet or whose daemon is not connected has no answer to
/// give. A report missing this section is far better than a list route that
/// fails because one machine of several is offline.
async fn storage_view(
    conn: &mut AsyncPgConnection,
    state: &AppState,
    host: &Host,
) -> Option<StorageResponse> {
    let root = host.user_data_root.as_deref()?;
    let tenancy = crate::environments::reach_tenancy(state, host).await.ok()?;
    let capacity = tenancy.capacity(std::path::Path::new(root)).await.ok()?;
    // Derived from the reading just taken rather than asked for again: the
    // two are the same `statvfs`, and this route runs once per host listed.
    let ceiling = service_hosts::ceiling_of(capacity);
    let committed = service_hosts::projected_storage(conn, host, None)
        .await
        .ok()?;

    Some(StorageResponse {
        total_bytes: capacity.total_bytes as i64,
        available_bytes: capacity.available_bytes as i64,
        ceiling_bytes: ceiling,
        committed_bytes: committed,
    })
}

async fn host_response(
    conn: &mut AsyncPgConnection,
    state: &AppState,
    host: &Host,
) -> ApiResult<ServiceHostResponse> {
    let tenants = service_hosts::tenants(conn, host.id).await?.len() as i64;
    let storage = storage_view(conn, state, host).await;
    let agent = AgentResponse {
        connected: state.agents.get(host.id).is_some(),
        enrolled_at: crate::agent::routes::enrolled_at(conn, host.id).await?,
    };

    Ok(ServiceHostResponse {
        id: host.id,
        name: host.name.clone(),
        docker_endpoint: host.docker_endpoint.clone(),
        user_data_root: host.user_data_root.clone(),
        container_root_uid: host.container_root_uid,
        created_at: host.created_at,
        disabled_at: host.disabled_at,
        defaults: Limits::from(host),
        tenants,
        storage,
        agent,
    })
}

// ---------------------------------------------------------------------------
// Hosts
// ---------------------------------------------------------------------------

/// Registers a machine faber operates.
///
/// Written as `agent` + `docker`. A service host is a machine faber reaches
/// through the daemon installed on it, not the machine the API process
/// happens to be running on — that premise died when the API became a
/// container, where `systemctl` and `xfs_quota` are absent from the image and
/// `/sys/fs/cgroup` is the container's own subtree rather than the host's.
/// `docker_endpoint` therefore names the socket *on the agent's machine*.
///
/// The row is only half the registration. A host created here has no daemon
/// until an administrator runs the command `POST .../agent/enrollment`
/// returns, on the machine itself, as root. Until then the host exists and is
/// unreachable, which the storage section of its report shows by being
/// absent.
///
/// The machine has to be prepared first: cgroup v2 with the controllers
/// delegated down to faber's tenant slice, docker on the systemd cgroup
/// driver, and `user_data_root` on a filesystem mounted with project quotas.
/// None of that is something faber does at runtime, and its absence surfaces
/// as a confusing launch failure rather than as a refusal here.
async fn create_host(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
    Json(input): Json<CreateHostRequest>,
) -> ApiResult<(StatusCode, Json<ServiceHostResponse>)> {
    let name = input.name.trim();
    validate_name(name)?;

    let endpoint = input.docker_endpoint.trim();
    if endpoint.is_empty() {
        return Err(AppError::BadRequest(
            "docker_endpoint is required: faber never uses the process's ambient docker context"
                .into(),
        ));
    }
    validate_service_endpoint(endpoint)?;

    let data_root = input.user_data_root.trim();
    if data_root.is_empty() {
        return Err(AppError::BadRequest(
            "user_data_root is required: it is where each tenant's quota'd directory lives".into(),
        ));
    }
    validate_data_root(data_root)?;
    validate_container_root_uid(input.container_root_uid)?;
    validate_limits(&input.defaults)?;

    let new_host = NewHost {
        id: Uuid::now_v7(),
        // No owner. That is the entire marker of a service host — there is no
        // `kind` column and no flag, because a flag that can disagree with
        // ownership is a bug surface.
        user_id: None,
        name,
        transport: Transport::Agent.as_str(),
        exec_mode: ExecMode::Docker.as_str(),
        ssh_address: None,
        ssh_key_ref: None,
        ssh_host_key: None,
        docker_endpoint: Some(endpoint),
        // A service host is reached through its containers only. Binding a
        // session to the machine itself would put an agent on the filesystem
        // every tenant shares.
        root_path: None,
        default_cpu_millis: input.defaults.cpu_millis,
        default_memory_bytes: input.defaults.memory_bytes,
        default_storage_bytes: input.defaults.storage_bytes,
        default_container_max: input.defaults.container_max,
        user_data_root: Some(data_root),
        container_root_uid: Some(i64::from(input.container_root_uid)),
        preview_network: None,
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
            ) => AppError::BadRequest(format!("a service host named '{name}' already exists")),
            other => AppError::db(other, "admin.hosts.create"),
        })?;

    let body = host_response(&mut conn, &state, &inserted).await?;
    Ok((StatusCode::CREATED, Json(body)))
}

/// Every service host, drained ones included — the user-facing list hides a
/// drained host because there is nothing a user could do about it, and this
/// list exists for the person who can.
async fn list_hosts(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
) -> ApiResult<Json<Vec<ServiceHostResponse>>> {
    let mut conn = state.db.get().await?;

    let hosts: Vec<Host> = host::table
        .filter(host::user_id.is_null())
        .order(host::created_at.asc())
        .select(Host::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "admin.hosts.list"))?;

    let mut responses = Vec::with_capacity(hosts.len());
    for found in &hosts {
        responses.push(host_response(&mut conn, &state, found).await?);
    }
    Ok(Json(responses))
}

async fn fetch_host(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ServiceHostResponse>> {
    let mut conn = state.db.get().await?;
    let found = service_host(&mut conn, id).await?;
    Ok(Json(host_response(&mut conn, &state, &found).await?))
}

/// Edits a service host, including the defaults every tenant without a grant
/// of their own resolves to.
///
/// Raising `default_storage_bytes` is a grant raise for every one of those
/// tenants at once, so it goes through the same reservation check a single
/// grant does — storage is the one resource faber does not overcommit, and a
/// mass raise is the easiest way to overcommit it by accident. After the write
/// the new limits are pushed to the machine for every materialised tenant,
/// because a limit that only exists in the database is not a limit.
async fn update_host(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateHostRequest>,
) -> ApiResult<Json<ServiceHostResponse>> {
    let mut conn = state.db.get().await?;
    let current = service_host(&mut conn, id).await?;

    let name = trimmed(input.name.as_deref());
    if let Some(name) = name {
        validate_name(name)?;
    }
    let endpoint = trimmed(input.docker_endpoint.as_deref());
    if let Some(endpoint) = endpoint {
        validate_service_endpoint(endpoint)?;
    }
    let data_root = trimmed(input.user_data_root.as_deref());
    if let Some(root) = data_root {
        validate_data_root(root)?;
        // Every tenant's directory, project quota, and work in progress sits
        // under the old root. Moving the root would leave all of it where it
        // is and quietly hand them empty directories somewhere else.
        if root != current.user_data_root.as_deref().unwrap_or_default()
            && !service_hosts::tenants(&mut conn, current.id)
                .await?
                .is_empty()
        {
            return Err(AppError::Conflict(
                "this host already has tenants; their data lives under the current \
                 user_data_root and faber does not move it"
                    .into(),
            ));
        }
    }
    if let Some(uid) = input.container_root_uid {
        validate_container_root_uid(uid)?;
    }
    if let Some(ref limits) = input.defaults {
        validate_limits(limits)?;
    }

    if let Some(limits) = input.defaults {
        // The host as it would be, asked what it would then cost.
        let prospective = Host {
            default_cpu_millis: limits.cpu_millis,
            default_memory_bytes: limits.memory_bytes,
            default_storage_bytes: limits.storage_bytes,
            default_container_max: limits.container_max,
            ..current.clone()
        };
        let tenancy = crate::environments::reach_tenancy(&state, &current).await?;
        service_hosts::check_storage_capacity(&mut conn, &tenancy, &current, &prospective, None)
            .await?;
    }

    let patch = UpdateHost {
        name,
        docker_endpoint: endpoint.map(Some),
        user_data_root: data_root.map(Some),
        container_root_uid: input.container_root_uid.map(|uid| Some(i64::from(uid))),
        default_cpu_millis: input.defaults.map(|l| l.cpu_millis),
        default_memory_bytes: input.defaults.map(|l| l.memory_bytes),
        default_storage_bytes: input.defaults.map(|l| l.storage_bytes),
        default_container_max: input.defaults.map(|l| l.container_max),
        disabled_at: input.disabled.map(|d| d.then(Utc::now)),
        ..Default::default()
    };

    let updated: Host = diesel::update(
        host::table
            .filter(host::id.eq(id))
            .filter(host::user_id.is_null()),
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
        ) => AppError::BadRequest("a service host with that name already exists".into()),
        other => AppError::db(other, "admin.hosts.update"),
    })?;

    if input.defaults.is_some() {
        reapply_all(&mut conn, &state, &updated).await;
    }

    Ok(Json(host_response(&mut conn, &state, &updated).await?))
}

/// Drops the registration. The machine is untouched, and so is everything on
/// it — including any tenant directory, which is why this refuses while anyone
/// is materialised: the row that says "this user's storage is reserved here"
/// would go with it, and their data would be left with nothing pointing at it.
///
/// `PATCH { disabled: true }` is the reversible alternative and the one an
/// operator almost always wants: it stops new launches and leaves what is
/// running alone.
async fn delete_host(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;
    let found = service_host(&mut conn, id).await?;

    let tenants = service_hosts::tenants(&mut conn, found.id).await?;
    if !tenants.is_empty() {
        return Err(AppError::Conflict(format!(
            "{} user(s) are materialised on this host; drain it with disabled instead",
            tenants.len()
        )));
    }

    diesel::delete(
        host::table
            .filter(host::id.eq(found.id))
            .filter(host::user_id.is_null()),
    )
    .execute(&mut conn)
    .await
    .map_err(|err| AppError::db(err, "admin.hosts.delete"))?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// The daemon on a service host
// ---------------------------------------------------------------------------

/// Issues the one-time command that installs a service host's daemon.
///
/// The user-facing counterpart of this route lives in `agent::routes` and is
/// gated on owning the host, which by construction can never match a service
/// host — `user_id IS NULL` matches no owner. So this is not a convenience
/// wrapper: without it a service host created by `create_host` would have no
/// way to acquire a daemon at all.
///
/// That split is also the whole of the separability service-host credentials
/// need. Their blast radius is genuinely larger than a user host's — the
/// daemon on the other end runs as root on a machine several tenants share —
/// so issuing one has to be an administrator's act and revoking one has to be
/// possible without touching anybody else's. Both fall out of the route
/// gating rather than out of a second table or a flag on `agent_credential`
/// that could disagree with the host it points at.
///
/// The command it hands back installs a *system* unit and says so out loud,
/// because that authority is the only reason the daemon can write a cgroup
/// limit or a project quota. Re-issuing supersedes an unredeemed token; it
/// does not revoke a daemon already enrolled, which is what the DELETE below
/// is for.
async fn enroll_agent(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<crate::agent::routes::EnrollResponse>)> {
    let mut conn = state.db.get().await?;
    let found = service_host(&mut conn, id).await?;

    if found.transport != Transport::Agent.as_str() {
        return Err(AppError::BadRequest(
            "this host is not reached over an agent daemon".into(),
        ));
    }

    let issued = crate::agent::routes::issue_enrollment(
        &state,
        &mut conn,
        found.id,
        crate::agent::routes::Privilege::System,
    )
    .await?;

    Ok((StatusCode::CREATED, Json(issued)))
}

/// Revokes a service host's daemon credential and drops its connection.
///
/// Distinct from draining the host (`disabled_at`) and from deleting the
/// registration: this says the credential itself should stop working — a
/// machine being rebuilt, or a token believed stolen. A stolen service-host
/// token matters more than a user's, because "newest wins" preemption means
/// whoever holds it can displace the real daemon and receive that host's
/// launches, including the tenant limits faber believes it is enforcing.
///
/// Nothing else changes. The tenants, their reservations, and their
/// directories are all untouched; the host simply has no daemon until one
/// enrolls again.
async fn revoke_agent(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;
    let found = service_host(&mut conn, id).await?;

    let revoked = crate::agent::routes::revoke_credential(&state, &mut conn, found.id).await?;
    if revoked == 0 {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Tenants and grants
// ---------------------------------------------------------------------------

/// Everyone materialised on a host, with what they are allowed and what they
/// are using.
///
/// Materialised, not registered: a tenant appears here the first time they
/// launch something, because that is when their directory is created and their
/// storage reserved. Summing reservations over accounts that merely exist
/// would be unbounded and would reserve storage for people who never arrive.
async fn list_tenants(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<TenantResponse>>> {
    let mut conn = state.db.get().await?;
    let found = service_host(&mut conn, id).await?;
    let rows = service_hosts::tenants(&mut conn, found.id).await?;

    let data_root = found.user_data_root.clone().map(std::path::PathBuf::from);
    // One connection to the host for the whole listing, and `None` if it has
    // no daemon on the wire: a tenant list that fails outright because a
    // machine is offline tells an operator less than one whose usage columns
    // are empty.
    let tenancy = crate::environments::reach_tenancy(&state, &found)
        .await
        .ok();

    let mut responses = Vec::with_capacity(rows.len());
    for tenant in rows {
        let subject = service_hosts::subject(&mut conn, tenant.user_id).await?;
        let resolved = service_hosts::resolve_quota(&mut conn, &found, tenant.user_id).await?;
        let containers =
            service_hosts::live_container_count(&mut conn, found.id, tenant.user_id).await?;
        let note = grant_note(&mut conn, found.id, tenant.user_id).await?;

        // Read from the machine, never stored: per-user aggregates are a
        // property of the cgroup and the project quota, and a copy in the
        // database would be the same cached-liveness mistake the host schema
        // already refuses.
        let usage = match (tenancy.as_ref(), data_root.as_deref(), subject) {
            (Some(tenancy), Some(root), Some(subject)) => tenancy.usage(root, subject).await,
            _ => Default::default(),
        };

        responses.push(TenantResponse {
            user_id: tenant.user_id,
            subject_id: subject,
            materialised_at: tenant.created_at,
            containers,
            quota: ResolvedQuotaResponse {
                limits: Limits {
                    cpu_millis: resolved.quota.cpu_millis,
                    memory_bytes: resolved.quota.memory_bytes,
                    storage_bytes: resolved.quota.storage_bytes,
                    container_max: resolved.quota.container_max,
                },
                granted: resolved.overridden,
                expires_at: resolved.expires_at,
                note: if resolved.overridden { note } else { None },
            },
            usage: UsageResponse {
                memory_bytes: usage.memory_bytes,
                pids: usage.pids,
                storage_bytes: usage.storage_bytes,
            },
        });
    }

    Ok(Json(responses))
}

/// Releases somebody's materialisation on a host, destroying their data on it.
///
/// **This removes their work.** The machine-side half is a project quota set to
/// zero and an `rm -rf` of their directory, so there is no retention window and
/// no export — releasing is the end of that data, and the reservation it was
/// holding goes back to the host the moment the row is tombstoned.
///
/// Refused while they still hold container registrations, which
/// [`release_host_user`](service_hosts::release_host_user) checks: the
/// directory is what those containers are running inside.
///
/// The host has to be reachable. Everywhere else in this module a machine-side
/// failure is logged and left for the next launch to repair, because the
/// database is the source of truth and the machine is the copy. Here the
/// ordering is the opposite way round: tombstoning the row while the bytes are
/// still on disk drops the reservation that `projected_storage` sums over, and
/// the host would then promise storage it has already spent. So an offline
/// daemon is a refusal rather than a partial release.
///
/// Any live grant is left alone. A grant is an administrator's decision about a
/// person with its own retire-and-audit lifecycle, not a property of their
/// directory — and if the same user materialises here again, the ceiling
/// somebody deliberately gave them is the one they should get.
async fn release_tenant(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
    Path((id, user_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;
    let found = service_host(&mut conn, id).await?;

    // Checked before the release rather than reading it out of the result: the
    // release itself is idempotent by design and returns cleanly for a user
    // who was never here, which as an answer to `DELETE .../users/{id}` would
    // report success for a typo'd id.
    if service_hosts::live_host_user(&mut conn, found.id, user_id)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound);
    }

    let tenancy = crate::environments::reach_tenancy(&state, &found).await?;
    service_hosts::release_host_user(&mut conn, &tenancy, &found, user_id).await?;

    Ok(StatusCode::NO_CONTENT)
}

/// The note on the live grant, if there is one.
async fn grant_note(
    conn: &mut AsyncPgConnection,
    host_id: Uuid,
    user_id: Uuid,
) -> ApiResult<Option<String>> {
    let note: Option<Option<String>> = host_user_quota::table
        .filter(host_user_quota::host_id.eq(host_id))
        .filter(host_user_quota::user_id.eq(user_id))
        .filter(host_user_quota::retired_at.is_null())
        .select(host_user_quota::note)
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "admin.grants.note"))?;
    Ok(note.flatten())
}

/// Gives one user their own ceiling on one host, replacing whatever they had.
///
/// Replacing, not amending. At most one grant per (host, user) is live at a
/// time — the previous one is retired rather than deleted, so what was given
/// and when stays answerable after the fact — and the live row is the resolved
/// quota in full. Sending only `storage_bytes` therefore grants unlimited CPU,
/// memory, and containers, which is exactly what a whole-row replacement
/// means and why this is a `PUT`.
///
/// A raise is checked against the filesystem before it is written, and the new
/// limits are pushed to the machine after — a grant that only exists in the
/// database is a promise nothing enforces.
async fn grant_quota(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path((id, user_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<GrantRequest>,
) -> ApiResult<Json<ResolvedQuotaResponse>> {
    validate_limits(&input.limits)?;
    if let Some(expires_at) = input.expires_at
        && expires_at <= Utc::now()
    {
        return Err(AppError::BadRequest(
            "expires_at is in the past, which would make this grant lapse the moment it is made"
                .into(),
        ));
    }

    let mut conn = state.db.get().await?;
    let found = service_host(&mut conn, id).await?;
    let target = known_user(&mut conn, user_id).await?;

    // The reservation check that materialisation runs, applied to the other
    // way the same number moves. It only bites for a tenant already
    // materialised — nothing is reserved for a user who has never launched
    // anything, and their first launch runs the check itself.
    let tenancy = crate::environments::reach_tenancy(&state, &found).await?;
    service_hosts::check_storage_capacity(
        &mut conn,
        &tenancy,
        &found,
        &found,
        Some((target, input.limits.storage_bytes)),
    )
    .await?;

    let limits = input.limits;
    let note = input
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    let expires_at = input.expires_at;
    let granted_by = admin.id;

    conn.transaction::<_, AppError, _>(|conn| {
        async move {
            retire_live_grant(conn, id, target).await?;

            diesel::insert_into(host_user_quota::table)
                .values(&NewHostUserQuota {
                    host_id: id,
                    user_id: target,
                    cpu_millis: limits.cpu_millis,
                    memory_bytes: limits.memory_bytes,
                    storage_bytes: limits.storage_bytes,
                    container_max: limits.container_max,
                    granted_by: Some(granted_by),
                    expires_at,
                    note,
                })
                .execute(conn)
                .await
                .map_err(|err| AppError::db(err, "admin.grants.insert"))?;

            Ok(())
        }
        .scope_boxed()
    })
    .await?;

    reapply_one(&mut conn, &state, &found, target).await;
    resolved_response(&mut conn, &found, target).await.map(Json)
}

/// Retires a user's grant, dropping them back to the host's defaults.
///
/// The defaults can be *higher* than the grant being removed, so this takes
/// the same reservation check a raise does rather than assuming a revocation
/// only ever frees space.
async fn revoke_quota(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
    Path((id, user_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<ResolvedQuotaResponse>> {
    let mut conn = state.db.get().await?;
    let found = service_host(&mut conn, id).await?;
    let target = known_user(&mut conn, user_id).await?;

    let tenancy = crate::environments::reach_tenancy(&state, &found).await?;
    service_hosts::check_storage_capacity(
        &mut conn,
        &tenancy,
        &found,
        &found,
        Some((target, found.default_storage_bytes)),
    )
    .await?;

    let retired = retire_live_grant(&mut conn, found.id, target).await?;
    if retired == 0 {
        return Err(AppError::NotFound);
    }

    reapply_one(&mut conn, &state, &found, target).await;
    resolved_response(&mut conn, &found, target).await.map(Json)
}

/// Tombstones the live grant. Retired rather than deleted: the partial unique
/// index leaves room for exactly one live grant per pair, and what was granted
/// to whom stays answerable afterwards.
async fn retire_live_grant(
    conn: &mut AsyncPgConnection,
    host_id: Uuid,
    user_id: Uuid,
) -> ApiResult<usize> {
    diesel::update(
        host_user_quota::table
            .filter(host_user_quota::host_id.eq(host_id))
            .filter(host_user_quota::user_id.eq(user_id))
            .filter(host_user_quota::retired_at.is_null()),
    )
    .set(host_user_quota::retired_at.eq(Some(Utc::now())))
    .execute(conn)
    .await
    .map_err(|err| AppError::db(err, "admin.grants.retire"))
}

/// The user the grant names, or a 400 saying so.
///
/// A grant against an id nobody holds would insert cleanly against the foreign
/// key only if the row exists; checking first turns a constraint violation
/// into a sentence.
async fn known_user(conn: &mut AsyncPgConnection, user_id: Uuid) -> ApiResult<Uuid> {
    users::table
        .filter(users::id.eq(user_id))
        .select(users::id)
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "admin.grants.user"))?
        .ok_or_else(|| AppError::BadRequest("no such user".into()))
}

async fn resolved_response(
    conn: &mut AsyncPgConnection,
    host: &Host,
    user_id: Uuid,
) -> ApiResult<ResolvedQuotaResponse> {
    let resolved = service_hosts::resolve_quota(conn, host, user_id).await?;
    let note = grant_note(conn, host.id, user_id).await?;

    Ok(ResolvedQuotaResponse {
        limits: Limits {
            cpu_millis: resolved.quota.cpu_millis,
            memory_bytes: resolved.quota.memory_bytes,
            storage_bytes: resolved.quota.storage_bytes,
            container_max: resolved.quota.container_max,
        },
        granted: resolved.overridden,
        expires_at: resolved.expires_at,
        note: if resolved.overridden { note } else { None },
    })
}

/// Pushes a user's now-resolved limits onto the machine.
///
/// Only for a tenant who is materialised: applying to anyone else would create
/// their directory and their slice before they have ever asked for one, which
/// is the eager reservation the lazy materialisation exists to avoid.
///
/// Failures are logged rather than reported. The grant is already written and
/// is the source of truth; the machine is brought back in line by the next
/// launch, which reapplies the same limits idempotently — the same reason
/// there is no reconciler daemon.
async fn reapply_one(conn: &mut AsyncPgConnection, state: &AppState, host: &Host, user_id: Uuid) {
    match service_hosts::live_host_user(conn, host.id, user_id).await {
        Ok(Some(_)) => service_hosts::reapply(conn, state, &[(host.id, user_id)]).await,
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(host = %host.id, %user_id, %error, "could not tell whether a grant needs applying")
        }
    }
}

/// The same, for every materialised tenant — what a change to the host's
/// defaults touches.
async fn reapply_all(conn: &mut AsyncPgConnection, state: &AppState, host: &Host) {
    let rows = match service_hosts::tenants(conn, host.id).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(host = %host.id, %error, "could not list tenants to reapply defaults");
            return;
        }
    };

    let pairs: Vec<(Uuid, Uuid)> = rows
        .iter()
        .map(|tenant| (host.id, tenant.user_id))
        .collect();
    service_hosts::reapply(conn, state, &pairs).await;
}

// ---------------------------------------------------------------------------
// Service images
// ---------------------------------------------------------------------------

/// Creates a template faber provides.
///
/// A service host runs these and nothing else: letting a user name an
/// arbitrary registry reference on faber's machine is arbitrary code, plus
/// unbounded pull bandwidth, plus image layers sitting outside every project
/// quota. The consequence is that a shared host with no service image on it is
/// a machine nobody can spawn on, which is why this lives here rather than
/// waiting to be someone's SQL.
async fn create_image(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
    Json(input): Json<CreateImageRequest>,
) -> ApiResult<(StatusCode, Json<ImageResponse>)> {
    let name = input.name.trim();
    validate_name(name)?;

    let reference = input.reference.trim();
    if reference.is_empty() {
        return Err(AppError::BadRequest("reference is required".into()));
    }

    let default_root_path = input.default_root_path.trim();
    if !default_root_path.starts_with('/') {
        return Err(AppError::BadRequest(
            "default_root_path must be absolute".into(),
        ));
    }

    let mut conn = state.db.get().await?;
    let inserted: Image = diesel::insert_into(image::table)
        .values(&crate::models::host::NewImage {
            id: Uuid::now_v7(),
            // No owner, by the same rule that marks a service host.
            user_id: None,
            name,
            reference,
            default_mounts: input.default_mounts,
            default_root_path,
        })
        .returning(Image::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| match err {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => AppError::BadRequest(format!("a service image named '{name}' already exists")),
            other => AppError::db(other, "admin.images.create"),
        })?;

    Ok((StatusCode::CREATED, Json(image_response(&inserted))))
}

async fn list_images(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
) -> ApiResult<Json<Vec<ImageResponse>>> {
    let mut conn = state.db.get().await?;

    let rows: Vec<Image> = image::table
        .filter(image::user_id.is_null())
        .order(image::created_at.asc())
        .select(Image::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "admin.images.list"))?;

    Ok(Json(rows.iter().map(image_response).collect()))
}

async fn update_image(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateImageRequest>,
) -> ApiResult<Json<ImageResponse>> {
    if let Some(name) = input.name.as_deref() {
        validate_name(name.trim())?;
    }
    if let Some(reference) = input.reference.as_deref()
        && reference.trim().is_empty()
    {
        return Err(AppError::BadRequest("reference cannot be empty".into()));
    }
    if let Some(root) = input.default_root_path.as_deref()
        && !root.trim().starts_with('/')
    {
        return Err(AppError::BadRequest(
            "default_root_path must be absolute".into(),
        ));
    }

    let mut conn = state.db.get().await?;
    let updated: Image = diesel::update(
        image::table
            .filter(image::id.eq(id))
            .filter(image::user_id.is_null()),
    )
    .set(UpdateImage {
        name: input.name.as_deref().map(str::trim),
        reference: input.reference.as_deref().map(str::trim),
        default_mounts: input.default_mounts,
        default_root_path: input.default_root_path.as_deref().map(str::trim),
    })
    .returning(Image::as_returning())
    .get_result(&mut conn)
    .await
    .map_err(|err| match err {
        diesel::result::Error::NotFound => AppError::NotFound,
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => AppError::BadRequest("a service image with that name already exists".into()),
        other => AppError::db(other, "admin.images.update"),
    })?;

    Ok(Json(image_response(&updated)))
}

/// Deletes the template. Containers spawned from it keep running — the link
/// back to an image is provenance and nothing resolves through it.
async fn delete_image(
    State(state): State<AppState>,
    AdminUser(_): AdminUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;

    let deleted = diesel::delete(
        image::table
            .filter(image::id.eq(id))
            .filter(image::user_id.is_null()),
    )
    .execute(&mut conn)
    .await
    .map_err(|err| AppError::db(err, "admin.images.delete"))?;

    if deleted == 0 {
        return Err(AppError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_carries_every_limit_and_reads_an_absent_one_as_unlimited() {
        // The whole-row rule, at the wire: what is missing from the body is
        // unlimited, not "leave the default in place". A client that means to
        // change one number has to send the other three back.
        let body: GrantRequest = serde_json::from_str(r#"{"storage_bytes":1024}"#).unwrap();
        assert_eq!(body.limits.storage_bytes, Some(1024));
        assert_eq!(body.limits.cpu_millis, None);
        assert_eq!(body.limits.memory_bytes, None);
        assert_eq!(body.limits.container_max, None);
    }

    #[test]
    fn a_zero_limit_is_refused_rather_than_stored() {
        // Zero cores is a host the tenant cannot use, which is never what
        // somebody meant to type — unlimited is spelled `null`.
        assert!(
            validate_limits(&Limits {
                cpu_millis: Some(0),
                ..Default::default()
            })
            .is_err()
        );
        assert!(validate_limits(&Limits::default()).is_ok());
    }
}
