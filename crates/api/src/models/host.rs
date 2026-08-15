//! Execution environments — see `internal-docs/host.md`.
//!
//! The host is the registration primitive: every execution mode bottoms out in
//! *reach the machine, then exec*, and only the machine carries authentication
//! and a network path. Containers hang off a host; probes observe one.
//!
//! Nothing here caches liveness. `disabled_at` is operator intent, and
//! `host_probe` is an append-only observation log whose rows are advisory —
//! the authoritative answer to "is it reachable" is the next connection
//! attempt.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::schema::{
    host, host_container, host_probe, host_user, host_user_quota, image, user_subject,
};

/// How faber reaches the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Local,
    Ssh,
    /// A daemon that dialed out to faber, rather than a host faber dials —
    /// see `internal-docs/agent-transport.md`. Carries no `ssh_address`;
    /// its identity lives in `agent_credential`, keyed by `host_id`.
    Agent,
}

impl Transport {
    pub fn as_str(&self) -> &'static str {
        match self {
            Transport::Local => "local",
            Transport::Ssh => "ssh",
            Transport::Agent => "agent",
        }
    }
}

/// What faber execs into once it has reached the machine.
///
/// Deliberately not derived from `docker_endpoint is not null`: an SSH host that
/// *could* run docker but is deliberately used direct is a real configuration,
/// and collapsing the two loses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecMode {
    Direct,
    Docker,
}

impl ExecMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecMode::Direct => "direct",
            ExecMode::Docker => "docker",
        }
    }
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = host)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Host {
    pub id: Uuid,
    /// `None` marks a *service host* — one faber operates rather than a user.
    /// That is the entire marker; there is no `kind` column and no
    /// `is_service` flag, because a flag that can disagree with ownership is a
    /// bug surface. Every write path filters `user_id = $me` and NULL never
    /// matches, so "users cannot edit service hosts" is derived rather than
    /// enforced.
    pub user_id: Option<Uuid>,
    pub name: String,
    pub transport: String,
    pub exec_mode: String,
    pub ssh_address: Option<String>,
    pub ssh_key_ref: Option<String>,
    /// The SHA256 fingerprint this host is known by. `None` until the first
    /// successful connection, which stores what it saw; every connection after
    /// verifies against it.
    pub ssh_host_key: Option<String>,
    pub docker_endpoint: Option<String>,
    pub created_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
    /// The agent-visible root when this host is used in direct mode. `None`
    /// means the host cannot be bound directly — `/` by default would hand an
    /// agent the whole machine because nobody filled in a field.
    pub root_path: Option<String>,

    /// Default per-user limits for this host. `None` is unlimited, everywhere
    /// and always — never "inherit", which is what keeps an override row a
    /// wholesale replacement rather than a field-level merge.
    pub default_cpu_millis: Option<i32>,
    pub default_memory_bytes: Option<i64>,
    pub default_storage_bytes: Option<i64>,
    pub default_container_max: Option<i32>,
    /// Parent of the per-user directories a service host quotas. Required for
    /// a service host by CHECK, and meaningless for an owned one.
    pub user_data_root: Option<String>,
}

impl Host {
    /// Operated by faber rather than by a user.
    pub fn service(&self) -> bool {
        self.user_id.is_none()
    }
}

#[derive(Insertable)]
#[diesel(table_name = host)]
pub struct NewHost<'a> {
    pub id: Uuid,
    /// `None` creates a service host, which no user API path does — service
    /// hosts are provisioned by an operator.
    pub user_id: Option<Uuid>,
    pub name: &'a str,
    pub transport: &'a str,
    pub exec_mode: &'a str,
    pub ssh_address: Option<&'a str>,
    pub ssh_key_ref: Option<&'a str>,
    pub ssh_host_key: Option<&'a str>,
    pub docker_endpoint: Option<&'a str>,
    pub root_path: Option<&'a str>,
}

#[derive(AsChangeset, Default)]
#[diesel(table_name = host)]
pub struct UpdateHost<'a> {
    pub name: Option<&'a str>,
    pub transport: Option<&'a str>,
    pub exec_mode: Option<&'a str>,
    pub ssh_address: Option<Option<&'a str>>,
    pub ssh_key_ref: Option<Option<&'a str>>,
    /// `Some(None)` clears it, which is how a rebuilt machine is re-trusted:
    /// deliberately, by the operator, rather than automatically on mismatch.
    pub ssh_host_key: Option<Option<&'a str>>,
    pub docker_endpoint: Option<Option<&'a str>>,
    /// Operator intent. `Some(None)` re-enables; the column never reflects an
    /// observation.
    pub disabled_at: Option<Option<DateTime<Utc>>>,
    pub root_path: Option<Option<&'a str>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = host_container)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct HostContainer {
    pub id: Uuid,
    pub host_id: Uuid,
    /// Who this container belongs to. On a shared host the owner cannot be
    /// derived from `host.user_id`, so it is recorded here — which also makes
    /// the per-user count check a single indexed predicate with no join.
    pub user_id: Uuid,
    pub container_ref: String,
    pub name: Option<String>,
    pub root_path: String,
    pub created_at: DateTime<Utc>,
    pub unregistered_at: Option<DateTime<Utc>>,
    /// When faber created this container. `None` means the user did and faber
    /// was only told about it — which is the difference between a container
    /// faber may destroy and one it must leave alone.
    pub managed_at: Option<DateTime<Utc>>,
    /// The template it was created from, kept as provenance. Nothing resolves
    /// through it, and it goes null if the template is deleted.
    pub image_id: Option<Uuid>,
}

impl HostContainer {
    /// Faber created it, so faber may destroy it.
    pub fn managed(&self) -> bool {
        self.managed_at.is_some()
    }
}

#[derive(Insertable)]
#[diesel(table_name = host_container)]
pub struct NewHostContainer<'a> {
    pub id: Uuid,
    pub host_id: Uuid,
    pub user_id: Uuid,
    pub container_ref: &'a str,
    pub name: Option<&'a str>,
    pub root_path: &'a str,
    pub managed_at: Option<DateTime<Utc>>,
    pub image_id: Option<Uuid>,
}

#[derive(AsChangeset, Default)]
#[diesel(table_name = host_container)]
pub struct UpdateHostContainer<'a> {
    pub container_ref: Option<&'a str>,
    pub name: Option<Option<&'a str>>,
    pub root_path: Option<&'a str>,
    /// State of the *registration*, not of the container. A container the user
    /// removed out of band stays registered until someone says otherwise.
    pub unregistered_at: Option<Option<DateTime<Utc>>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = host_probe)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct HostProbe {
    pub id: Uuid,
    pub host_id: Uuid,
    pub container_id: Option<Uuid>,
    pub probed_at: DateTime<Utc>,
    pub ok: bool,
    pub error: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub shell: Option<String>,
    pub tools: Option<Value>,
    pub root_path: Option<String>,
}

#[derive(Insertable)]
#[diesel(table_name = host_probe)]
pub struct NewHostProbe<'a> {
    pub id: Uuid,
    pub host_id: Uuid,
    pub container_id: Option<Uuid>,
    pub ok: bool,
    pub error: Option<&'a str>,
    pub os: Option<&'a str>,
    pub arch: Option<&'a str>,
    pub shell: Option<&'a str>,
    pub tools: Option<Value>,
    pub root_path: Option<&'a str>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = image)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Image {
    pub id: Uuid,
    /// `None` marks a *service image*, by the same rule that marks a service
    /// host. Service hosts accept only these: letting a user run an arbitrary
    /// reference on faber's machine is arbitrary code plus unbounded pull
    /// bandwidth plus image layers sitting outside any project quota.
    pub user_id: Option<Uuid>,
    pub name: String,
    pub reference: String,
    pub default_mounts: Option<Value>,
    pub default_root_path: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = image)]
pub struct NewImage<'a> {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub name: &'a str,
    pub reference: &'a str,
    pub default_mounts: Option<Value>,
    pub default_root_path: &'a str,
}

#[derive(AsChangeset, Default)]
#[diesel(table_name = image)]
pub struct UpdateImage<'a> {
    pub name: Option<&'a str>,
    pub reference: Option<&'a str>,
    pub default_mounts: Option<Option<Value>>,
    pub default_root_path: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// Shared-host tenancy.
// ---------------------------------------------------------------------------

/// A user's stable 32-bit identity on every service host.
///
/// One integer serving two purposes — the XFS project ID that carries their
/// storage quota, and the host-side UID their containers run as. Allocated
/// once per user and reused on every host, so audit lines from different
/// machines join directly. It is drawn from a sequence rather than hashed from
/// the user's UUID, because a 32-bit hash collides around 77k users and a
/// collision here is two users sharing a storage quota.
#[derive(Debug, Clone, Copy, Queryable, Selectable)]
#[diesel(table_name = user_subject)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct UserSubject {
    pub user_id: Uuid,
    pub subject_id: i32,
    pub created_at: DateTime<Utc>,
}

/// A user materialised on a host: their directory exists and their storage is
/// reserved against the filesystem.
///
/// The row *is* the reservation. Materialised lazily on first use rather than
/// eagerly for every account, because sum-over-all-users is unbounded on a
/// service host and sum-over-materialised-users is countable.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = host_user)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct HostUser {
    pub id: Uuid,
    pub host_id: Uuid,
    pub user_id: Uuid,
    pub created_at: DateTime<Utc>,
    /// Set when the reservation is returned. A tombstone rather than a delete,
    /// matching `disabled_at` / `unregistered_at` / `revoked_at`.
    pub released_at: Option<DateTime<Utc>>,
}

#[derive(Insertable)]
#[diesel(table_name = host_user)]
pub struct NewHostUser {
    pub host_id: Uuid,
    pub user_id: Uuid,
}

/// One user's quota override on one host.
///
/// A live row is the resolved quota *in full* — it replaces the host defaults
/// wholesale rather than merging field by field. That is what makes
/// override-to-unlimited expressible: `None` here means unlimited, exactly as
/// it does on `host`, and never "fall back to the default".
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = host_user_quota)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct HostUserQuota {
    pub id: Uuid,
    pub host_id: Uuid,
    pub user_id: Uuid,
    pub cpu_millis: Option<i32>,
    pub memory_bytes: Option<i64>,
    pub storage_bytes: Option<i64>,
    pub container_max: Option<i32>,
    pub granted_at: DateTime<Utc>,
    /// Who granted it. No foreign key: a grant stays auditable after the
    /// admin who made it is gone.
    pub granted_by: Option<Uuid>,
    /// Honoured the instant it passes, by the read path rather than by the
    /// sweeper — so a stalled sweeper can fail to revoke promptly but can
    /// never grant extra.
    pub expires_at: Option<DateTime<Utc>>,
    pub retired_at: Option<DateTime<Utc>>,
    pub note: Option<String>,
}

#[derive(Insertable)]
#[diesel(table_name = host_user_quota)]
pub struct NewHostUserQuota<'a> {
    pub host_id: Uuid,
    pub user_id: Uuid,
    pub cpu_millis: Option<i32>,
    pub memory_bytes: Option<i64>,
    pub storage_bytes: Option<i64>,
    pub container_max: Option<i32>,
    pub granted_by: Option<Uuid>,
    pub expires_at: Option<DateTime<Utc>>,
    pub note: Option<&'a str>,
}
