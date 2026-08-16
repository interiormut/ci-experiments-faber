//! Hosts faber operates, and the per-user ceilings that make sharing one safe.
//!
//! A **service host** is a host with no
//! owner — `host.user_id IS NULL` is the entire marker, and there is no `kind`
//! column and no `is_service` flag, because a flag that can disagree with
//! ownership is a bug surface. It behaves exactly like `transport='local'` +
//! `exec_mode='docker'`; the only new thing is that many users share it.
//!
//! Two rules run through everything below and are worth stating once:
//!
//! **Quota is a ceiling on a claim, never a reservation.** Per-user limits do
//! not sum to machine capacity and must not be assumed to. Storage is the sole
//! exception, because `ENOSPC` is global and cannot be repaired by the user
//! who caused it, while CPU and RAM merely degrade under contention.
//!
//! **Enforcement lives in the kernel; accounting does not live in the
//! database.** Faber stores grants and never measured usage. Per-user
//! aggregates are a property of the parent cgroup and the XFS project quota,
//! and are read live — the same rule that keeps host liveness out of the
//! schema.
//!
//! **The machine is at the other end of a link, and the reads that decide an
//! admission happen before the lock.** A service host is reached through the
//! agent daemon installed on it, so every one of the calls below that touches
//! the machine is a round trip rather than a syscall. That is why
//! [`storage_ceiling`] is read by the caller and passed *into* [`admit`]:
//! holding the per-(host, user) advisory lock across a network call would let
//! one wedged link hold a database transaction open for a full command
//! timeout.

use chrono::{DateTime, Utc};
use diesel::{
    BoolExpressionMethods, ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper,
};
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use environment::tenancy::Tenancy;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::{
    error::{ApiResult, AppError},
    models::host::{Host, HostUser, HostUserQuota, NewHostUser},
    schema::{host, host_container, host_user, host_user_quota, user_subject},
};

/// Reserve kept free on a service host's filesystem, as a fraction of its
/// total size. Storage is reserved rather than overcommitted, and the reserve
/// is what keeps the last accepted reservation from being the one that fills
/// the disk.
const STORAGE_RESERVE_RATIO: f64 = 0.10;

/// Absolute floor on available memory, as a fraction of physical. Below it,
/// new launches are refused regardless of the requester's grant.
const MEMORY_FLOOR_RATIO: f64 = 0.15;

/// `pids.max` for a user slice. Generous enough for a real build — a parallel
/// `cargo build` alone runs into the hundreds — and tight enough that a fork
/// bomb hits a wall. Empirical, and the first number here to want tuning.
const DEFAULT_PIDS_MAX: i32 = 4096;

/// Per-container upper-layer cap, a *secondary* mechanism to the project
/// quota. Safe to state as a per-container number only because the count
/// limit exists: worst case is `container_max × this`.
pub const CONTAINER_LAYER_BYTES: i64 = 10 * 1024 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------

/// Picks the service host a user's next container should go on.
///
/// A seam, not a scheduler. There is one service host today and this returns
/// it; the point of having the function now is that everything downstream is
/// written host-agnostic from the start.
///
/// **Placement is sticky per user.** A user's storage lives on one host's
/// filesystem, and sending them elsewhere silently abandons their workspace —
/// so a host where they are already materialised wins over any other
/// consideration. Rebalancing is therefore a migration problem rather than a
/// scheduling one, and quota being per (host, user) is tolerable precisely
/// because stickiness keeps the host count at one in practice.
///
/// `disabled_at` doubles as the drain marker: no new launches, and containers
/// already running are left alone.
pub async fn select_host(conn: &mut AsyncPgConnection, user_id: Uuid) -> ApiResult<Option<Host>> {
    let hosts: Vec<Host> = host::table
        .filter(host::user_id.is_null())
        .filter(host::disabled_at.is_null())
        .order(host::created_at.asc())
        .select(Host::as_select())
        .load(conn)
        .await
        .map_err(|err| AppError::db(err, "service_hosts.select_host"))?;

    if hosts.is_empty() {
        return Ok(None);
    }

    let ids: Vec<Uuid> = hosts.iter().map(|host| host.id).collect();
    let materialised: Vec<Uuid> = host_user::table
        .filter(host_user::host_id.eq_any(&ids))
        .filter(host_user::user_id.eq(user_id))
        .filter(host_user::released_at.is_null())
        .select(host_user::host_id)
        .load(conn)
        .await
        .map_err(|err| AppError::db(err, "service_hosts.select_host.sticky"))?;

    let sticky = hosts
        .iter()
        .find(|host| materialised.contains(&host.id))
        .cloned();

    Ok(sticky.or_else(|| hosts.into_iter().next()))
}

// ---------------------------------------------------------------------------
// Subject identity
// ---------------------------------------------------------------------------

/// The user's stable 32-bit id, allocating one if they have never needed it.
///
/// One integer per user, reused on every host — so a UID in a container, a
/// project id on a filesystem, and a line in an audit log from a different
/// machine all name the same person without a translation layer. Allocated
/// from a sequence rather than hashed from the user's UUID: a 32-bit hash
/// collides around 77k users, and a collision here is two users sharing a
/// storage quota.
pub async fn ensure_subject(conn: &mut AsyncPgConnection, user_id: Uuid) -> ApiResult<i32> {
    if let Some(existing) = subject(conn, user_id).await? {
        return Ok(existing);
    }

    // `ON CONFLICT DO NOTHING` then re-read, rather than an upsert returning
    // the row: two concurrent launches for one user must not both consume a
    // sequence value and then disagree about which one is the user's.
    diesel::insert_into(user_subject::table)
        .values(user_subject::user_id.eq(user_id))
        .on_conflict_do_nothing()
        .execute(conn)
        .await
        .map_err(|err| AppError::db(err, "service_hosts.ensure_subject.insert"))?;

    subject(conn, user_id).await?.ok_or_else(|| {
        tracing::error!(%user_id, "subject id vanished immediately after allocation");
        AppError::Internal
    })
}

/// The user's subject id if they have one, without allocating.
///
/// The read counterpart to [`ensure_subject`]: reporting on a host must not
/// hand an id to a user who has never needed one.
pub async fn subject(conn: &mut AsyncPgConnection, user_id: Uuid) -> ApiResult<Option<i32>> {
    user_subject::table
        .filter(user_subject::user_id.eq(user_id))
        .select(user_subject::subject_id)
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "service_hosts.subject"))
}

// ---------------------------------------------------------------------------
// Quota resolution
// ---------------------------------------------------------------------------

/// One user's resolved limits on one host. `None` is unlimited.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Quota {
    pub cpu_millis: Option<i32>,
    pub memory_bytes: Option<i64>,
    pub storage_bytes: Option<i64>,
    pub container_max: Option<i32>,
}

impl Quota {
    /// The host's defaults, which are the answer whenever no override is live.
    fn defaults(host: &Host) -> Self {
        Quota {
            cpu_millis: host.default_cpu_millis,
            memory_bytes: host.default_memory_bytes,
            storage_bytes: host.default_storage_bytes,
            container_max: host.default_container_max,
        }
    }

    /// The machine-side shape of the same numbers.
    pub fn limits(&self) -> environment::tenancy::Limits {
        environment::tenancy::Limits {
            cpu_millis: self.cpu_millis,
            memory_bytes: self.memory_bytes,
            storage_bytes: self.storage_bytes,
            pids_max: Some(DEFAULT_PIDS_MAX),
        }
    }
}

/// A user's resolved quota on a host, and where it came from.
#[derive(Clone, Debug)]
pub struct Resolution {
    pub quota: Quota,
    /// True when a live override row supplied it. Carried so an operator
    /// reading a report can tell a default from a grant without a second
    /// query.
    pub overridden: bool,
    /// When the grant lapses, if it is temporary. Surfaced because immediate
    /// expiry means a grant can shrink underneath a running workload.
    pub expires_at: Option<DateTime<Utc>>,
}

/// Resolves one user's quota on one host.
///
/// A live, unexpired override row is the answer **in full**; otherwise the
/// host defaults are the answer in full; otherwise unlimited. The two are
/// never mixed. That is row-level replacement rather than a field-level merge,
/// and it is the only thing that makes "override this user to unlimited"
/// expressible at all — a per-field fallback would read the override's NULL as
/// "inherit" and hand back the default.
///
/// Expiry is honoured here rather than waiting for the sweeper, so a stalled
/// sweeper can fail to revoke promptly but can never grant extra.
pub async fn resolve_quota(
    conn: &mut AsyncPgConnection,
    host: &Host,
    user_id: Uuid,
) -> ApiResult<Resolution> {
    let live: Option<HostUserQuota> = host_user_quota::table
        .filter(host_user_quota::host_id.eq(host.id))
        .filter(host_user_quota::user_id.eq(user_id))
        .filter(host_user_quota::retired_at.is_null())
        .filter(
            host_user_quota::expires_at
                .is_null()
                .or(host_user_quota::expires_at.gt(Utc::now())),
        )
        .select(HostUserQuota::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "service_hosts.resolve_quota"))?;

    Ok(match live {
        Some(grant) => Resolution {
            quota: Quota {
                cpu_millis: grant.cpu_millis,
                memory_bytes: grant.memory_bytes,
                storage_bytes: grant.storage_bytes,
                container_max: grant.container_max,
            },
            overridden: true,
            expires_at: grant.expires_at,
        },
        None => Resolution {
            quota: Quota::defaults(host),
            overridden: false,
            expires_at: None,
        },
    })
}

// ---------------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------------

/// Registered containers this user holds on this host.
///
/// Count bills on *existence*, not on running: a stopped container still holds
/// its quota'd directory, so it still counts. Modelling it any other way would
/// need a lifecycle column faber would then have to keep true.
pub async fn live_container_count(
    conn: &mut AsyncPgConnection,
    host_id: Uuid,
    user_id: Uuid,
) -> ApiResult<i64> {
    host_container::table
        .filter(host_container::host_id.eq(host_id))
        .filter(host_container::user_id.eq(user_id))
        .filter(host_container::unregistered_at.is_null())
        .count()
        .get_result(conn)
        .await
        .map_err(|err| AppError::db(err, "service_hosts.live_container_count"))
}

/// The advisory-lock key for one (host, user) pair.
///
/// Without the lock, two concurrent launches both observe `count < max` and
/// both pass. The key is derived rather than stored because it has to be the
/// same number in every process for the lock to mean anything.
fn admission_key(host_id: Uuid, user_id: Uuid) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in host_id.as_bytes().iter().chain(user_id.as_bytes()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash as i64
}

/// Takes the (host, user) admission lock for the rest of the transaction.
async fn lock_admission(
    conn: &mut AsyncPgConnection,
    host_id: Uuid,
    user_id: Uuid,
) -> ApiResult<()> {
    diesel::sql_query("SELECT pg_advisory_xact_lock($1)")
        .bind::<diesel::sql_types::BigInt, _>(admission_key(host_id, user_id))
        .execute(conn)
        .await
        .map_err(|err| AppError::db(err, "service_hosts.lock_admission"))?;
    Ok(())
}

/// Where one user's data lives on a service host.
pub fn user_paths(host: &Host, subject: i32) -> ApiResult<environment::tenancy::UserPaths> {
    let root = data_root(host)?;
    Ok(environment::tenancy::UserPaths::under(&root, subject))
}

/// A service host's data root, or a refusal naming what is missing.
pub fn data_root(host: &Host) -> ApiResult<PathBuf> {
    host.user_data_root
        .as_deref()
        .map(PathBuf::from)
        .ok_or_else(|| {
            tracing::error!(host = %host.id, "service host has no user_data_root");
            AppError::Internal
        })
}

/// Materialises a user on a host, reserving their storage against the
/// filesystem.
///
/// Creating the `host_user` row **is** the reservation. It is done lazily, on
/// first use, rather than eagerly for every account, because sum-over-all-
/// users is unbounded on a service host and sum-over-materialised-users is
/// countable — which is also why there is no "max users per host" field: it
/// falls out of this check.
///
/// Returns `HostAtCapacity` rather than `QuotaExceeded` when there is no room:
/// nobody in the session can act on it, and dressing it up as a quota error
/// would invite an agent to start deleting things that would not help.
pub async fn ensure_host_user(
    conn: &mut AsyncPgConnection,
    host: &Host,
    user_id: Uuid,
    quota: &Quota,
    ceiling: i64,
) -> ApiResult<HostUser> {
    if let Some(existing) = live_host_user(conn, host.id, user_id).await? {
        return Ok(existing);
    }

    let wanted = quota.storage_bytes.unwrap_or(0);
    if wanted > 0 {
        // The ceiling was read before the lock was taken and is deliberately
        // a moment stale. Nothing is lost by that: the lock is per-(host,
        // user), so it never serialised this check against another user's
        // launch in the first place — the filesystem's size was always being
        // read concurrently with someone else's reservation. What *is*
        // serialised is the sum below, which is the part that must not
        // interleave with itself.
        let committed = projected_storage(conn, host, None).await?;

        if committed + wanted > ceiling {
            return Err(AppError::HostAtCapacity {
                resource: "storage",
            });
        }
    }

    diesel::insert_into(host_user::table)
        .values(&NewHostUser {
            host_id: host.id,
            user_id,
        })
        .on_conflict_do_nothing()
        .execute(conn)
        .await
        .map_err(|err| AppError::db(err, "service_hosts.ensure_host_user.insert"))?;

    live_host_user(conn, host.id, user_id)
        .await?
        .ok_or_else(|| {
            tracing::error!(host = %host.id, %user_id, "host_user vanished after insert");
            AppError::Internal
        })
}

pub async fn live_host_user(
    conn: &mut AsyncPgConnection,
    host_id: Uuid,
    user_id: Uuid,
) -> ApiResult<Option<HostUser>> {
    host_user::table
        .filter(host_user::host_id.eq(host_id))
        .filter(host_user::user_id.eq(user_id))
        .filter(host_user::released_at.is_null())
        .select(HostUser::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "service_hosts.live_host_user"))
}

/// Everyone materialised on this host, oldest first.
pub async fn tenants(conn: &mut AsyncPgConnection, host_id: Uuid) -> ApiResult<Vec<HostUser>> {
    host_user::table
        .filter(host_user::host_id.eq(host_id))
        .filter(host_user::released_at.is_null())
        .order(host_user::created_at.asc())
        .select(HostUser::as_select())
        .load(conn)
        .await
        .map_err(|err| AppError::db(err, "service_hosts.tenants"))
}

/// The most storage this filesystem may have promised out at once.
///
/// The reserve is what keeps the last accepted reservation from being the one
/// that fills the disk — storage is the single resource faber refuses to
/// overcommit, because running out of it is global and cannot be repaired by
/// the user who caused it.
pub async fn storage_ceiling(tenancy: &Tenancy, root: &Path) -> ApiResult<i64> {
    let capacity = tenancy
        .capacity(root)
        .await
        .map_err(crate::environments::fault)?;
    Ok(ceiling_of(capacity))
}

/// The same number from a reading already taken.
///
/// Split from [`storage_ceiling`] because the read is a round trip now: a
/// caller that already has the filesystem's size — an operator report showing
/// both what the disk holds and what has been promised out of it — would
/// otherwise ask the host the same question twice, once per host listed.
pub fn ceiling_of(capacity: environment::tenancy::Capacity) -> i64 {
    (capacity.total_bytes as f64 * (1.0 - STORAGE_RESERVE_RATIO)) as i64
}

/// Storage promised on this host, summed over materialised users.
///
/// Computed from live rows joined to their *resolved* quotas rather than from
/// a snapshot column, so raising a grant cannot drift from the reservation it
/// implies. The row count is bounded by exactly this check, which is what
/// makes loading them all reasonable.
///
/// `host` is read for its defaults, so passing a host with the defaults a
/// pending patch would write answers "what would this cost afterwards" —
/// which is the question, because changing a default silently regrants every
/// tenant who has no override of their own. `grant` substitutes one user's
/// storage for the value they are about to be given.
pub async fn projected_storage(
    conn: &mut AsyncPgConnection,
    host: &Host,
    grant: Option<(Uuid, Option<i64>)>,
) -> ApiResult<i64> {
    let live = tenants(conn, host.id).await?;

    let mut total: i64 = 0;
    for tenant in live {
        let bytes = match grant {
            Some((user_id, bytes)) if user_id == tenant.user_id => bytes,
            _ => {
                resolve_quota(conn, host, tenant.user_id)
                    .await?
                    .quota
                    .storage_bytes
            }
        };
        total = total.saturating_add(bytes.unwrap_or(0));
    }
    Ok(total)
}

/// Refuses a change that would promise more storage than the filesystem holds.
///
/// The check `ensure_host_user` runs when a tenant first appears, applied to
/// the other two ways the same number moves: a grant raised for one user, and
/// a host default raised for everyone who has no grant of their own. Without
/// it, storage is reserved on the way in and overcommitted on every path
/// afterwards.
///
/// A change that lowers the total is always allowed, even on a host already
/// over its ceiling — a filesystem that shrank under an existing set of grants
/// is a real situation, and refusing the reductions that would repair it would
/// be exactly backwards.
pub async fn check_storage_capacity(
    conn: &mut AsyncPgConnection,
    tenancy: &Tenancy,
    current: &Host,
    prospective: &Host,
    grant: Option<(Uuid, Option<i64>)>,
) -> ApiResult<()> {
    let root = data_root(prospective)?;
    let ceiling = storage_ceiling(tenancy, &root).await?;

    let after = projected_storage(conn, prospective, grant).await?;
    if after <= ceiling {
        return Ok(());
    }

    let before = projected_storage(conn, current, None).await?;
    if after > before {
        return Err(AppError::HostAtCapacity {
            resource: "storage",
        });
    }
    Ok(())
}

/// Refuses a launch when the machine's free memory is under the floor.
///
/// A floor check, not a fit check. `free >= grant` is meaningless under
/// overcommit — it would defeat the overcommit it exists to manage — so this
/// refuses everyone below an absolute reserve rather than refusing the
/// requester whose grant does not fit. Safety does not depend on it either
/// way: `faber.slice` bounds the tenant tree in the kernel, and this check
/// exists so the first failure is a clean refusal rather than an OOM kill
/// somewhere unrelated.
pub async fn check_memory_floor(tenancy: &Tenancy) -> ApiResult<()> {
    let memory = tenancy.memory().await.map_err(crate::environments::fault)?;

    let floor = (memory.total_bytes as f64 * MEMORY_FLOOR_RATIO) as u64;
    if memory.available_bytes < floor {
        return Err(AppError::HostAtCapacity { resource: "memory" });
    }
    Ok(())
}

/// Everything a launch has to settle before the machine is touched, decided
/// under the (host, user) admission lock in one transaction.
pub struct Admitted {
    pub subject: i32,
    pub quota: Quota,
    pub container_id: Uuid,
}

/// Runs the admission half of a launch: lock, check the count, reserve the
/// storage, and write the container row.
///
/// The row is written *inside* the lock deliberately. The count check is only
/// race-free if the row that will make the next check fail already exists when
/// the lock releases — which means the row precedes the container, and a
/// launch that then fails on the machine tombstones it with
/// `unregistered_at`. `container_ref` therefore has to be a name faber chooses
/// up front and passes to `docker run`, not the id the daemon returns.
///
/// `ceiling` arrives already read, from [`storage_ceiling`], because reading
/// it is a round trip to the host and this function holds a database lock for
/// its whole body. Nothing about the check weakens: see [`ensure_host_user`].
#[allow(clippy::too_many_arguments)]
pub async fn admit(
    conn: &mut AsyncPgConnection,
    host: &Host,
    user_id: Uuid,
    container_ref: &str,
    container_name: &str,
    root_path: &str,
    image_id: Uuid,
    ceiling: i64,
) -> ApiResult<Admitted> {
    let host = host.clone();
    let container_ref = container_ref.to_owned();
    let container_name = container_name.to_owned();
    let root_path = root_path.to_owned();

    conn.transaction::<_, AppError, _>(move |conn| {
        async move {
            lock_admission(conn, host.id, user_id).await?;

            let subject = ensure_subject(conn, user_id).await?;
            let resolved = resolve_quota(conn, &host, user_id).await?;

            if let Some(max) = resolved.quota.container_max {
                let held = live_container_count(conn, host.id, user_id).await?;
                if held >= i64::from(max) {
                    return Err(AppError::QuotaExceeded {
                        resource: "containers",
                        limit: Some(i64::from(max)),
                        used: Some(held),
                        // Bindings are append-only and an agent cannot drop an
                        // environment, so there is nothing here for one to
                        // reclaim — the user or their harness has to.
                        reclaimable_path: None,
                    });
                }
            }

            ensure_host_user(conn, &host, user_id, &resolved.quota, ceiling).await?;

            let container_id = Uuid::now_v7();
            diesel::insert_into(host_container::table)
                .values(&crate::models::host::NewHostContainer {
                    id: container_id,
                    host_id: host.id,
                    user_id,
                    container_ref: &container_ref,
                    name: Some(&container_name),
                    root_path: &root_path,
                    managed_at: Some(Utc::now()),
                    image_id: Some(image_id),
                })
                .execute(conn)
                .await
                .map_err(|err| match err {
                    // The name is taken by a container that is still live —
                    // the user's own, or another tenant's, since the index is
                    // per host. Their mistake to fix, so it is theirs to read:
                    // before the index went partial this arrived as a bare
                    // unique violation, and half the time from a tombstone
                    // they had no way to see.
                    diesel::result::Error::DatabaseError(
                        diesel::result::DatabaseErrorKind::UniqueViolation,
                        _,
                    ) => AppError::BadRequest(format!(
                        "'{container_ref}' is already in use on this host"
                    )),
                    other => AppError::db(other, "service_hosts.admit.insert_container"),
                })?;

            Ok(Admitted {
                subject,
                quota: resolved.quota,
                container_id,
            })
        }
        .scope_boxed()
    })
    .await
}

/// Tombstones a container row whose launch did not finish.
///
/// The slice and the project quota are left in place: they are per-user, not
/// per-container, and the next launch reuses them.
pub async fn withdraw(conn: &mut AsyncPgConnection, container_id: Uuid) {
    let result = diesel::update(host_container::table.filter(host_container::id.eq(container_id)))
        .set(host_container::unregistered_at.eq(Some(Utc::now())))
        .execute(conn)
        .await;

    if let Err(error) = result {
        // Reported to the operator rather than to the caller, who is already
        // being told why their launch failed. The row left behind holds a
        // count slot until someone clears it, which is visible and fixable.
        tracing::error!(%container_id, %error, "could not withdraw a container row after a failed launch");
    }
}

// ---------------------------------------------------------------------------
// Expiry
// ---------------------------------------------------------------------------

/// Retires grants whose `expires_at` has passed and returns whose limits now
/// need rewriting.
///
/// The read path already ignores an expired row, so this is the belt to that
/// braces: it cannot change what anyone is *entitled* to, only bring the
/// machine back in line with it. "Immediate" is therefore true in semantics
/// and about a sweep interval in effect, which is a gap for operator docs to
/// state rather than paper over.
pub async fn sweep_expired(conn: &mut AsyncPgConnection) -> ApiResult<Vec<(Uuid, Uuid)>> {
    let retired: Vec<(Uuid, Uuid)> = diesel::update(
        host_user_quota::table
            .filter(host_user_quota::retired_at.is_null())
            .filter(host_user_quota::expires_at.le(Utc::now())),
    )
    .set(host_user_quota::retired_at.eq(Some(Utc::now())))
    .returning((host_user_quota::host_id, host_user_quota::user_id))
    .load(conn)
    .await
    .map_err(|err| AppError::db(err, "service_hosts.sweep_expired"))?;

    Ok(retired)
}

/// How often expired grants are retired and their limits rewritten.
///
/// The interval is the whole distance between "immediate" in semantics and
/// immediate in effect. The read path already refuses to *grant* on an expired
/// row from the instant it lapses; this is only how long the machine can keep
/// honouring one, and it belongs in operator docs rather than being papered
/// over.
const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Runs the expiry sweep for as long as the process lives.
///
/// Deliberately not a reconciler. It reacts to one event — a grant lapsing —
/// and does nothing else; drift of any other kind is repaired by the next
/// container launch, which rewrites the same limits idempotently.
pub fn spawn_expiry_sweeper(state: crate::state::AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
        loop {
            ticker.tick().await;

            let mut conn = match state.db.get().await {
                Ok(conn) => conn,
                Err(error) => {
                    tracing::warn!(%error, "quota sweep could not get a connection");
                    continue;
                }
            };

            match sweep_expired(&mut conn).await {
                Ok(pairs) if pairs.is_empty() => {}
                Ok(pairs) => {
                    tracing::info!(count = pairs.len(), "retiring expired quota grants");
                    reapply(&mut conn, &state, &pairs).await;
                }
                Err(error) => tracing::warn!(%error, "quota sweep failed"),
            }
        }
    });
}

/// Reapplies the now-resolved limits for every pair a sweep retired.
///
/// Idempotent, and a missed sweep is repaired by the next one or by the next
/// container launch — which is the whole reason there is no reconciler daemon.
/// A host whose daemon is not connected fails here every sweep until it
/// returns, and that is the right noise: the limits it is failing to write are
/// the ones a lapsed grant should have taken away.
pub async fn reapply(
    conn: &mut AsyncPgConnection,
    state: &crate::state::AppState,
    pairs: &[(Uuid, Uuid)],
) {
    for (host_id, user_id) in pairs {
        if let Err(error) = reapply_one(conn, state, *host_id, *user_id).await {
            tracing::warn!(%host_id, %user_id, %error, "could not reapply limits after expiry");
        }
    }
}

async fn reapply_one(
    conn: &mut AsyncPgConnection,
    state: &crate::state::AppState,
    host_id: Uuid,
    user_id: Uuid,
) -> ApiResult<()> {
    let host_row: Host = host::table
        .filter(host::id.eq(host_id))
        .select(Host::as_select())
        .first(conn)
        .await
        .map_err(|err| AppError::db(err, "service_hosts.reapply.host"))?;

    if !host_row.service() {
        return Ok(());
    }

    let subject = ensure_subject(conn, user_id).await?;
    let resolved = resolve_quota(conn, &host_row, user_id).await?;
    let tenancy = crate::environments::reach_tenancy(state, &host_row).await?;
    apply(&tenancy, &host_row, subject, &resolved.quota).await
}

/// Writes a user's limits onto the machine: the slice, then the project quota.
///
/// Both writes are idempotent and both are meant to be repeated — at launch,
/// after a grant changes, and after an expiry sweep. Repetition is the repair
/// path, so a failure here is a failure of *this* launch and never a reason to
/// go looking for drift elsewhere.
pub async fn apply(tenancy: &Tenancy, host: &Host, subject: i32, quota: &Quota) -> ApiResult<()> {
    tenancy
        .ensure_slice(subject, &quota.limits())
        .await
        .map_err(crate::environments::fault)?;

    let root = data_root(host)?;
    tenancy
        .ensure_user_data(&root, subject, quota.storage_bytes)
        .await
        .map_err(crate::environments::fault)?;

    Ok(())
}

/// Releases a user's materialisation on a host, returning the reservation.
///
/// Refused while they still hold containers: the reservation is what their
/// directory sits inside, and removing it under a running container would take
/// the workspace out from under a session.
///
/// **Immediate removal is the data policy**, and it is this function that
/// decides it: the project quota goes to zero and the directory is deleted in
/// the same call that tombstones the row. Neither a retention window nor an
/// export is expressible here — both would need somewhere for the bytes to go
/// and a sweeper to eventually forget them, and neither exists. The two routes
/// that reach this (an administrator releasing anyone, a user releasing
/// themselves) both say so before they ask.
///
/// Idempotent on purpose: a user with no subject id never had a directory, and
/// an already-released row updates nothing. A caller that needs "there was
/// nobody here" to be an error checks [`live_host_user`] first.
pub async fn release_host_user(
    conn: &mut AsyncPgConnection,
    tenancy: &Tenancy,
    host: &Host,
    user_id: Uuid,
) -> ApiResult<()> {
    let held = live_container_count(conn, host.id, user_id).await?;
    if held > 0 {
        return Err(AppError::Conflict(format!(
            "this user still has {held} container(s) registered on that host"
        )));
    }

    let Some(subject) = subject(conn, user_id).await? else {
        return Ok(());
    };
    let root = data_root(host)?;

    tenancy
        .release_user_data(&root, subject)
        .await
        .map_err(crate::environments::fault)?;

    diesel::update(
        host_user::table
            .filter(host_user::host_id.eq(host.id))
            .filter(host_user::user_id.eq(user_id))
            .filter(host_user::released_at.is_null()),
    )
    .set(host_user::released_at.eq(Some(Utc::now())))
    .execute(conn)
    .await
    .map_err(|err| AppError::db(err, "service_hosts.release_host_user"))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Introspection
// ---------------------------------------------------------------------------

/// What a user is granted and what they are using, assembled for a bind.
///
/// This is the answer to a problem the error taxonomy cannot solve on its own:
/// the kernel returns `EDQUOT` to `write(2)` *inside* the container, so
/// `file.write` can translate it cleanly but `exec` cannot — `cargo build`
/// exits nonzero with a linker message, a nonzero exit is a result rather than
/// an error, and nothing annotates it. Rather than pattern-matching every
/// tool's out-of-space prose, faber gives the agent a cheap authoritative way
/// to check the hypothesis a confusing exit suggested.
///
/// Returns the grant only. Usage is read from the machine each time the agent
/// asks, which is why nothing here measures anything.
/// The [`Tenancy`] is attached here rather than derived at measure time
/// because the bound target's own transport reaches *into the container*: a
/// cgroup counter read through it is the container's view of itself, and the
/// question is what the tenant is using on the host.
///
/// The `host.service()` guard stays. A daemon installed under a user's own
/// account is physically incapable of writing a cgroup limit or reading a
/// tenant quota, and that is the backstop — but this is the gate, and faber
/// should not be *attempting* either against a machine somebody else owns.
pub async fn allowance(
    conn: &mut AsyncPgConnection,
    state: &crate::state::AppState,
    host: &Host,
    user_id: Uuid,
) -> ApiResult<Option<environment::tenancy::Allowance>> {
    if !host.service() {
        return Ok(None);
    }

    let Some(subject) = subject(conn, user_id).await? else {
        return Ok(None);
    };
    let resolved = resolve_quota(conn, host, user_id).await?;

    Ok(Some(environment::tenancy::Allowance {
        limits: resolved.quota.limits(),
        container_max: resolved.quota.container_max,
        subject,
        data_root: data_root(host)?,
        expires_at: resolved.expires_at.map(|at| at.to_rfc3339()),
        tenancy: crate::environments::reach_tenancy(state, host).await?,
    }))
}

/// Where a tenant's scratch directory lands inside the container.
///
/// Named because the spawn path has to refuse a `root_path` that would land
/// on top of it: two mounts with one destination is a launch the daemon
/// refuses, and refusing it up there is cheaper than a tombstoned row here.
pub const SCRATCH_PATH: &str = "/scratch";

/// The mounts every container on a service host gets.
pub fn mounts(paths: &environment::tenancy::UserPaths, root_path: &str) -> Vec<(String, String)> {
    vec![
        (path_string(&paths.work), root_path.to_owned()),
        (path_string(&paths.scratch), SCRATCH_PATH.to_owned()),
    ]
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_admission_key_does_not_depend_on_argument_order() {
        let host = Uuid::from_u128(1);
        let user = Uuid::from_u128(2);
        // The same pair has to hash to the same lock in every process, and a
        // different pair has to hash somewhere else — a shared key would
        // serialise unrelated launches, and a per-process one would serialise
        // nothing.
        assert_eq!(admission_key(host, user), admission_key(host, user));
        assert_ne!(admission_key(host, user), admission_key(user, host));
    }

    // -----------------------------------------------------------------------
    // Against a real Postgres
    // -----------------------------------------------------------------------
    //
    // The database is part of the correctness surface here, not a store the
    // logic happens to use: the partial unique indexes, the advisory lock, and
    // the row-level semantics of quota resolution are all enforced by
    // Postgres, and none of them can be observed against a mock. These skip
    // when `DATABASE_URL` is unset so a checkout with no database still runs
    // the suite green — a skipped test that says why beats a test that passes
    // by testing nothing.

    use crate::models::host::NewHostUserQuota;
    use diesel_async::AsyncConnection;

    async fn connection() -> Option<AsyncPgConnection> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let mut conn = AsyncPgConnection::establish(&url)
            .await
            .expect("DATABASE_URL is set but not connectable");
        // Everything below rolls back: these tests write real rows into
        // whatever database the developer is pointed at.
        conn.begin_test_transaction()
            .await
            .expect("could not open a test transaction");
        Some(conn)
    }

    async fn a_user(conn: &mut AsyncPgConnection) -> Uuid {
        let id = Uuid::now_v7();
        diesel::sql_query("INSERT INTO users (id, identity_id) VALUES ($1, $2)")
            .bind::<diesel::sql_types::Uuid, _>(id)
            .bind::<diesel::sql_types::Uuid, _>(Uuid::now_v7())
            .execute(conn)
            .await
            .expect("could not insert a user");
        id
    }

    /// Inserts a host, returning the database's complaint if it had one.
    async fn insert_host(
        conn: &mut AsyncPgConnection,
        owner: Option<Uuid>,
        name: &str,
    ) -> Result<Uuid, diesel::result::Error> {
        let id = Uuid::now_v7();
        diesel::sql_query(
            "INSERT INTO host (id, user_id, name, transport, exec_mode, docker_endpoint, \
             user_data_root) VALUES ($1, $2, $3, 'local', 'docker', 'unix:///var/run/docker.sock', \
             '/srv/faber')",
        )
        .bind::<diesel::sql_types::Uuid, _>(id)
        .bind::<diesel::sql_types::Nullable<diesel::sql_types::Uuid>, _>(owner)
        .bind::<diesel::sql_types::Text, _>(name.to_owned())
        .execute(conn)
        .await?;
        Ok(id)
    }

    async fn load_host(conn: &mut AsyncPgConnection, id: Uuid) -> Host {
        host::table
            .filter(host::id.eq(id))
            .select(Host::as_select())
            .first(conn)
            .await
            .expect("could not load the host just inserted")
    }

    #[tokio::test]
    async fn a_name_is_unique_among_service_hosts_and_free_across_owners() {
        let Some(mut conn) = connection().await else {
            return;
        };
        let one = a_user(&mut conn).await;
        let two = a_user(&mut conn).await;

        // Two users may each call their host `build`.
        insert_host(&mut conn, Some(one), "build").await.unwrap();
        insert_host(&mut conn, Some(two), "build").await.unwrap();

        // A service host may share that name too: it is in a different
        // namespace, not a colliding row.
        insert_host(&mut conn, None, "build").await.unwrap();

        // But only one service host may hold it. This is the case the old
        // table constraint got wrong — `UNIQUE (user_id, name)` treats every
        // NULL owner as distinct, so it would have allowed a second.
        let clash = insert_host(&mut conn, None, "build").await;
        assert!(
            matches!(
                clash,
                Err(diesel::result::Error::DatabaseError(
                    diesel::result::DatabaseErrorKind::UniqueViolation,
                    _
                ))
            ),
            "a second service host named 'build' was accepted",
        );
    }

    #[tokio::test]
    async fn a_withdrawn_launch_gives_its_name_back() {
        let Some(mut conn) = connection().await else {
            return;
        };
        let user = a_user(&mut conn).await;
        let host_id = insert_host(&mut conn, None, "shared").await.unwrap();

        async fn register(
            conn: &mut AsyncPgConnection,
            host_id: Uuid,
            user_id: Uuid,
            id: Uuid,
        ) -> Result<usize, diesel::result::Error> {
            diesel::insert_into(host_container::table)
                .values(&crate::models::host::NewHostContainer {
                    id,
                    host_id,
                    user_id,
                    container_ref: "faber-work",
                    name: Some("work"),
                    root_path: "/workspace",
                    managed_at: Some(Utc::now()),
                    image_id: None,
                })
                .execute(conn)
                .await
        }

        // A launch that died on the machine: the row was written under the
        // admission lock, then withdrawn when docker refused.
        let failed = Uuid::now_v7();
        register(&mut conn, host_id, user, failed).await.unwrap();
        withdraw(&mut conn, failed).await;

        // The retry has to work. Before the uniqueness went partial this was
        // a bare unique violation against a tombstone the user's listing does
        // not show, so the name was gone for good and the reason invisible.
        let second = Uuid::now_v7();
        register(&mut conn, host_id, user, second)
            .await
            .expect("a name held only by a withdrawn launch was still claimed");

        // And the invariant that matters is untouched: two *live* rows may
        // not name one container on one host.
        let third = register(&mut conn, host_id, user, Uuid::now_v7()).await;
        assert!(
            matches!(
                third,
                Err(diesel::result::Error::DatabaseError(
                    diesel::result::DatabaseErrorKind::UniqueViolation,
                    _
                ))
            ),
            "two live registrations were allowed to share a ref on one host",
        );
    }

    #[tokio::test]
    async fn an_override_replaces_the_defaults_wholesale() {
        let Some(mut conn) = connection().await else {
            return;
        };
        let user = a_user(&mut conn).await;
        let host_id = insert_host(&mut conn, None, "shared").await.unwrap();

        diesel::sql_query(
            "UPDATE host SET default_cpu_millis = 2000, default_memory_bytes = 8589934592, \
             default_storage_bytes = 53687091200, default_container_max = 3 WHERE id = $1",
        )
        .bind::<diesel::sql_types::Uuid, _>(host_id)
        .execute(&mut conn)
        .await
        .unwrap();

        let host = load_host(&mut conn, host_id).await;
        let defaults = resolve_quota(&mut conn, &host, user).await.unwrap();
        assert_eq!(defaults.quota.cpu_millis, Some(2000));
        assert_eq!(defaults.quota.container_max, Some(3));
        assert!(!defaults.overridden);

        // A grant raising storage and leaving everything else null. Under
        // field-level merge — which is what `COALESCE` in the resolution query
        // would produce — the other three would fall back to the host
        // defaults. They must not: null here means unlimited, and this is the
        // only way "give this user no CPU ceiling" is expressible at all.
        diesel::insert_into(host_user_quota::table)
            .values(&NewHostUserQuota {
                host_id,
                user_id: user,
                cpu_millis: None,
                memory_bytes: None,
                storage_bytes: Some(107_374_182_400),
                container_max: None,
                granted_by: None,
                expires_at: None,
                note: Some("test"),
            })
            .execute(&mut conn)
            .await
            .unwrap();

        let granted = resolve_quota(&mut conn, &host, user).await.unwrap();
        assert!(granted.overridden);
        assert_eq!(granted.quota.storage_bytes, Some(107_374_182_400));
        assert_eq!(granted.quota.cpu_millis, None);
        assert_eq!(granted.quota.memory_bytes, None);
        assert_eq!(granted.quota.container_max, None);
    }

    #[tokio::test]
    async fn an_expired_grant_stops_counting_the_moment_it_lapses() {
        let Some(mut conn) = connection().await else {
            return;
        };
        let user = a_user(&mut conn).await;
        let host_id = insert_host(&mut conn, None, "shared").await.unwrap();
        diesel::sql_query("UPDATE host SET default_container_max = 1 WHERE id = $1")
            .bind::<diesel::sql_types::Uuid, _>(host_id)
            .execute(&mut conn)
            .await
            .unwrap();

        diesel::insert_into(host_user_quota::table)
            .values(&NewHostUserQuota {
                host_id,
                user_id: user,
                cpu_millis: None,
                memory_bytes: None,
                storage_bytes: None,
                container_max: Some(50),
                granted_by: None,
                expires_at: Some(Utc::now() - chrono::Duration::seconds(1)),
                note: None,
            })
            .execute(&mut conn)
            .await
            .unwrap();

        // The row is still live — `retired_at` is null and no sweep has run —
        // and it is still ignored. That ordering is what keeps a stalled
        // sweeper from being able to grant anything: the worst it can do is
        // fail to revoke promptly.
        let host = load_host(&mut conn, host_id).await;
        let resolved = resolve_quota(&mut conn, &host, user).await.unwrap();
        assert!(!resolved.overridden);
        assert_eq!(resolved.quota.container_max, Some(1));

        let swept = sweep_expired(&mut conn).await.unwrap();
        assert!(swept.contains(&(host_id, user)));
    }

    #[tokio::test]
    async fn one_live_grant_per_pair_and_retired_ones_do_not_block_a_new_one() {
        let Some(mut conn) = connection().await else {
            return;
        };
        let user = a_user(&mut conn).await;
        let host_id = insert_host(&mut conn, None, "shared").await.unwrap();

        let grant = |note: &'static str| NewHostUserQuota {
            host_id,
            user_id: user,
            cpu_millis: Some(1000),
            memory_bytes: None,
            storage_bytes: None,
            container_max: None,
            granted_by: None,
            expires_at: None,
            note: Some(note),
        };

        diesel::insert_into(host_user_quota::table)
            .values(&grant("first"))
            .execute(&mut conn)
            .await
            .unwrap();

        // Inside a nested transaction, so the violation rolls back to a
        // savepoint instead of poisoning the outer one for the rest of the
        // test.
        let second = conn
            .transaction::<_, diesel::result::Error, _>(|conn| {
                async move {
                    diesel::insert_into(host_user_quota::table)
                        .values(&grant("second"))
                        .execute(conn)
                        .await
                }
                .scope_boxed()
            })
            .await;
        assert!(
            second.is_err(),
            "two live grants for one pair were accepted"
        );

        // Retiring is a tombstone, not a delete: the old grant stays auditable
        // and stops blocking the new one.
        diesel::update(host_user_quota::table.filter(host_user_quota::host_id.eq(host_id)))
            .set(host_user_quota::retired_at.eq(Some(Utc::now())))
            .execute(&mut conn)
            .await
            .unwrap();

        diesel::insert_into(host_user_quota::table)
            .values(&grant("third"))
            .execute(&mut conn)
            .await
            .expect("a retired grant blocked a new one");
    }

    /// A machine that fails the test if it is reached at all.
    ///
    /// Releasing writes a quota of zero and an `rm -rf`, so "the refusal came
    /// first" is not a detail of ordering — it is the difference between
    /// refusing to release a tenant and deleting their work and then refusing.
    struct Untouchable;

    #[async_trait::async_trait]
    impl environment::spawn::Spawn for Untouchable {
        async fn spawn(
            &self,
            run: environment::spawn::Run,
        ) -> Result<Box<dyn environment::spawn::Proc>, environment::Fault> {
            panic!("a release that should have been refused ran {:?}", run.argv);
        }
    }

    #[async_trait::async_trait]
    impl environment::tenancy::Reads for Untouchable {
        async fn capacity(
            &self,
            _path: &str,
        ) -> Result<environment::tenancy::Capacity, environment::Fault> {
            panic!("a release that should have been refused read the filesystem");
        }
        async fn memory(&self) -> Result<environment::tenancy::Memory, environment::Fault> {
            panic!("a release that should have been refused read memory");
        }
        async fn counters(
            &self,
            _paths: &[String],
        ) -> Result<Vec<Option<u64>>, environment::Fault> {
            panic!("a release that should have been refused read a counter");
        }
    }

    #[tokio::test]
    async fn releasing_is_refused_while_the_tenant_still_holds_a_container() {
        let Some(mut conn) = connection().await else {
            return;
        };
        let user = a_user(&mut conn).await;
        let host_id = insert_host(&mut conn, None, "shared").await.unwrap();
        let host = load_host(&mut conn, host_id).await;

        diesel::insert_into(host_user::table)
            .values(&NewHostUser {
                host_id,
                user_id: user,
            })
            .execute(&mut conn)
            .await
            .unwrap();
        diesel::insert_into(host_container::table)
            .values(&crate::models::host::NewHostContainer {
                id: Uuid::now_v7(),
                host_id,
                user_id: user,
                container_ref: "faber-test",
                name: None,
                root_path: "/work",
                managed_at: Some(Utc::now()),
                image_id: None,
            })
            .execute(&mut conn)
            .await
            .unwrap();

        let untouchable = Tenancy::new(
            std::sync::Arc::new(Untouchable),
            std::sync::Arc::new(Untouchable),
        );
        let refused = release_host_user(&mut conn, &untouchable, &host, user).await;
        assert!(
            matches!(refused, Err(AppError::Conflict(_))),
            "releasing a tenant with a container registered was allowed",
        );

        // And the row is still live, so the reservation it carries is still
        // being counted.
        assert!(
            live_host_user(&mut conn, host_id, user)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_released_tenant_stops_holding_its_reservation() {
        let Some(mut conn) = connection().await else {
            return;
        };
        let user = a_user(&mut conn).await;
        let host_id = insert_host(&mut conn, None, "shared").await.unwrap();
        diesel::sql_query("UPDATE host SET default_storage_bytes = 1000 WHERE id = $1")
            .bind::<diesel::sql_types::Uuid, _>(host_id)
            .execute(&mut conn)
            .await
            .unwrap();
        let host = load_host(&mut conn, host_id).await;

        diesel::insert_into(host_user::table)
            .values(&NewHostUser {
                host_id,
                user_id: user,
            })
            .execute(&mut conn)
            .await
            .unwrap();
        assert_eq!(
            projected_storage(&mut conn, &host, None).await.unwrap(),
            1000
        );

        // The tombstone is the whole of it: what the host has promised out is
        // summed over live rows, so releasing hands the space back without
        // anything having to adjust a total.
        diesel::update(host_user::table.filter(host_user::host_id.eq(host_id)))
            .set(host_user::released_at.eq(Some(Utc::now())))
            .execute(&mut conn)
            .await
            .unwrap();
        assert_eq!(projected_storage(&mut conn, &host, None).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn the_admission_lock_serialises_two_launches_for_one_pair() {
        // Two real connections, because a lock is only observable across them
        // — and no test transaction, because `pg_advisory_xact_lock` releases
        // with the transaction that took it.
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let host_id = Uuid::now_v7();
        let user_id = Uuid::now_v7();

        let mut first = AsyncPgConnection::establish(&url).await.unwrap();
        let mut second = AsyncPgConnection::establish(&url).await.unwrap();

        let key = admission_key(host_id, user_id);
        let held = std::sync::Arc::new(tokio::sync::Notify::new());
        let release = std::sync::Arc::new(tokio::sync::Notify::new());

        let holder = {
            let held = std::sync::Arc::clone(&held);
            let release = std::sync::Arc::clone(&release);
            tokio::spawn(async move {
                first
                    .transaction::<_, diesel::result::Error, _>(move |conn| {
                        async move {
                            diesel::sql_query("SELECT pg_advisory_xact_lock($1)")
                                .bind::<diesel::sql_types::BigInt, _>(key)
                                .execute(conn)
                                .await?;
                            held.notify_one();
                            release.notified().await;
                            Ok(())
                        }
                        .scope_boxed()
                    })
                    .await
            })
        };

        held.notified().await;

        // The second attempt must not get the lock while the first holds it.
        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            second.transaction::<_, diesel::result::Error, _>(|conn| {
                async move {
                    diesel::sql_query("SELECT pg_advisory_xact_lock($1)")
                        .bind::<diesel::sql_types::BigInt, _>(key)
                        .execute(conn)
                        .await?;
                    Ok(())
                }
                .scope_boxed()
            }),
        )
        .await;
        assert!(
            blocked.is_err(),
            "two launches for one (host, user) took the admission lock at once",
        );

        release.notify_one();
        holder.await.unwrap().unwrap();
    }
}
