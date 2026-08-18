# Deploying Faber

Faber is three deployable things and one prepared machine:

| Component | What it is | Where it runs |
|---|---|---|
| **API** (`crates/api`) | Axum server, Postgres, the whole domain | A container, anywhere |
| **UI** (`packages/faber-ui`) | Vite SPA served by a small Bun server | A container, anywhere |
| **Agent** (`crates/faber-agent`) | A daemon that dials *out* to the API | On every machine Faber reaches |
| **Service host** | A Linux box prepared to run many users' containers | Bare metal you control |

The agent is not deployed by you in the usual sense — the API builds it, serves
it, and hands out a one-line install command. See [Agents](#5-agents) and
[Service hosts](#6-service-hosts).

This is the only operational documentation tracked in this repository. There is
an `internal-docs/` directory with design records, but it is gitignored, so
nothing here defers to it.

---

## Contents

1. [Prerequisites](#1-prerequisites)
2. [The database](#2-the-database)
3. [The API](#3-the-api)
4. [The UI](#4-the-ui)
5. [Agents](#5-agents)
6. [Service hosts](#6-service-hosts) ← the long one
7. [Bootstrapping an administrator](#7-bootstrapping-an-administrator)
8. [Operating](#8-operating)
9. [Things that are not deployed](#9-things-that-are-not-deployed)

---

## 1. Prerequisites

- **PostgreSQL** — any recent version. `gen_random_uuid()` is used, so `pgcrypto`
  or PG13+.
- **A Surge instance** for authentication, or the test provider (development
  only — see [the warning](#surge_test_provider-is-a-security-boundary)).
- **Docker** with BuildKit, to build the two images.
- For a service host: **bare-metal Linux, x86_64**, with cgroup v2 and XFS. The
  x86_64 constraint is real and explained in [§5](#the-agent-binary-is-x86_64-only).

There is no orchestration in this repository — no compose file, no Helm chart,
no Terraform. The two Dockerfiles are the whole of the packaging; how they are
scheduled is yours.

---

## 2. The database

One Postgres database. Nothing to prepare beyond creating it and a role that
owns it.

```sh
createdb faber
```

**Migrations run automatically when the API starts.** They are compiled into the
binary with `embed_migrations!("migrations")` and applied against `DATABASE_URL`
before the server binds a port. There is no separate migrate step, no
`diesel migration run` in your deploy pipeline, and no image whose only job is
to migrate.

Two consequences worth planning for:

- **Deploy order is "point it at the database and start it."** If you were about
  to add a migration job, don't.
- **Two API replicas starting simultaneously against a fresh database will race
  on migrations.** Faber is designed to run as a single process; if you scale it
  horizontally you must start the first instance alone.

A migration failure is a hard panic before the listener binds, so a bad
migration is a container that will not come up rather than a server serving
wrongly.

---

## 3. The API

### Build

```sh
docker build -f crates/api/Dockerfile -t faber-api .
```

The build does more than compile the server. It produces, in one image:

- `faber-api` — a glibc binary built against `debian:bookworm-slim`.
- `faber-agent-x86_64` — a **statically linked musl** binary, built in a separate
  stage with its own cargo cache, landing in
  `/usr/local/share/faber/agent/faber-agent-x86_64`.

The agent is built here on purpose: the binary the API serves to an enrolling
host and the server that will talk to it are always the same release. It is
static (`crt-static` plus `relocation-model=static`) because agent transport
exists precisely for hosts whose libc Faber does not get to choose — a glibc
build from this image dies on Debian 11 with `GLIBC_2.34 not found`.

The image runs as the unprivileged `faber` user and exposes **3001**.

### Configuration

Everything is environment variables. Required ones panic at boot if absent,
which is deliberate — a Faber that starts with half its configuration is worse
than one that does not start.

#### Required

| Variable | Notes |
|---|---|
| `DATABASE_URL` | `postgres://user:pass@host/faber`. Also used for the migration connection. |
| `FABER_MASTER_KEY` | 32 random bytes, base64. Panics if missing, not valid base64, or not exactly 32 bytes. |
| `SURGE_SERVICE_TOKEN` | Required by the remote auth provider. Only optional under the test provider. |

Generate the master key once, and keep it:

```sh
head -c 32 /dev/urandom | base64
```

**Losing or rotating `FABER_MASTER_KEY` makes every stored credential
undecryptable.** It encrypts users' provider API keys and SSH key material at
rest. There is no re-wrap path — a rotation means every user re-enters every
credential. Treat it like a database encryption key, because it is one.

#### Auth

| Variable | Default | Notes |
|---|---|---|
| `SURGE_URL` | `http://localhost:3000` | The Surge server. |
| `SURGE_COOKIE_DOMAIN` | `.panit.dev` | **Change this.** The default is Panit's own domain. |
| `SURGE_AUTH_UI_ORIGIN` | `http://localhost:3000` | Origin serving the credential-entry UI. |
| `SURGE_SESSION_TTL_SECS` | `259200` (72h) | Must match upstream's. |

The browser-facing perimeter is mounted at **`/api/surge`** and reverse-proxies
to Surge, so the frontend only ever talks to Faber. The frontend's surge-client
`baseUrl` must match that prefix.

#### Networking

| Variable | Default | Notes |
|---|---|---|
| `API_PORT` | `3001` | Set to 3001 in the image already. |
| `CORS_ORIGIN` | *(none)* | Comma-separated. **Must list the UI's origin** or every page load resolves to signed-out. |
| `FABER_PUBLIC_URL` | *(none)* | This API's externally reachable base URL. |

`CORS_ORIGIN` is load-bearing twice: it governs Faber's own routes *and* is
passed through as the session zone's allowed origins. Leave it empty and the
frontend's `whoami` fails CORS, which the client cannot distinguish from an
unreachable auth perimeter — so the symptom is "nobody can log in", not a CORS
error anyone will notice.

`FABER_PUBLIC_URL` is optional at boot and every route works without it —
**except agent enrollment**, which cannot build an install command without
knowing what a machine out there should dial. `surge_url` is the auth service
and `cors_origins` names browsers; neither answers that question. If you skip
it, you get a healthy API and an unservable install command, discovered when you
try to add your first host.

#### Policy

| Variable | Default | Notes |
|---|---|---|
| `FABER_ALLOW_LOCAL_HOSTS` | **`true`** | Whether users may register hosts reached through the API process itself. |

The default is permissive: anything other than the literal string `false` enables
it. On a multi-user deployment **set this to `false`.** A local host is one
executed inside the API container, which means one user's run touching the API's
own filesystem and process namespace. Disabling it leaves existing local hosts
intact; it only refuses new ones. It is surfaced to the frontend at
`GET /api/config`.

#### Search (optional)

| Variable | Notes |
|---|---|
| `SEARXNG_URL` | One named SearXNG instance. Set it and every run gets the `search` tool. |
| `PARALLEL_API_KEY` | Parallel Search API key, used when no SearXNG instance is named. |
| `SEARCH_PUBLIC_NETWORK` | `true` to search the public SearXNG pool. Ignored when `SEARXNG_URL` is set. |
| `SEARCH_PROXY` | Outbound proxy, for search traffic only. |

`SEARCH_PROXY` is passed explicitly rather than read from `HTTPS_PROXY` because
this is a multi-user service and nothing about one user's run may be decided by
the host's ambient environment. The same principle governs Docker endpoints and
service-host configuration throughout.

#### `SURGE_TEST_PROVIDER` is a security boundary

`SURGE_TEST_PROVIDER=true` authenticates **every request as one fixed
identity**. It needs two locks turned:

1. The `test-provider` Cargo feature at build time.
2. The environment variable at run time.

**A production image must be built without the feature.** The standard
`crates/api/Dockerfile` does not enable it, so the environment variable alone
can do nothing — which is the intended arrangement. If you maintain a custom
build, do not carry the feature into it. The related `SURGE_TEST_USERNAME` and
`SURGE_TEST_DISPLAY_NAME` are development-only and have no production meaning.

Likewise, every `FABER_TEST_*` variable in the codebase configures the test
suite, not a deployment.

### Health

`GET /health` returns `{"ok": true}` with no authentication and no database
access. It answers "the process is up", not "the process is healthy" — it will
report ok while Postgres is unreachable. Use it as a liveness probe, not a
readiness one.

---

## 4. The UI

### Build

```sh
docker build -f packages/faber-ui/Dockerfile -t faber-ui .
```

Note the build context is the **repository root**, not the package directory.

### Configuration

The UI's configuration is **injected at runtime, not baked at build time**.
`server.ts` reads the environment on each request and inlines a
`window.__FABER_RUNTIME_CONFIG__` script into the HTML shell. One image
therefore serves every environment.

| Variable | Default | Notes |
|---|---|---|
| `FABER_API_URL` | `""` | Base URL of the API. Falls back to `API_URL`. Empty means same-origin. |
| `FABER_AUTH_MODE` | *(none)* | `inline` or `redirect`. Falls back to `AUTH_MODE`. |
| `PORT` | `3000` | |
| `HOST` | `0.0.0.0` | |

Two caching behaviours the server implements deliberately, worth knowing before
you put a CDN in front of it:

- `/assets/*` is served `immutable` with a one-year max-age, and a **miss there
  returns a genuine 404** rather than the HTML shell. Hashed assets are
  content-addressed; returning HTML would hand a stale client an opaque MIME
  error instead of a recoverable chunk-load failure.
- The shell itself is `no-store`, because it carries the injected runtime config.

### Wiring the two together

If the UI is on `https://faber.example.com` and the API on
`https://api.faber.example.com`:

```sh
# UI
FABER_API_URL=https://api.faber.example.com

# API
CORS_ORIGIN=https://faber.example.com
FABER_PUBLIC_URL=https://api.faber.example.com
SURGE_COOKIE_DOMAIN=.faber.example.com
```

Same-origin behind one proxy is simpler and avoids the CORS question entirely:
leave `FABER_API_URL` empty and route `/api/*` to the API.

---

## 5. Agents

An agent is a daemon on a machine Faber reaches. It **dials out** — there is no
inbound port to open on the target and no firewall change to make.

Everything that runs on that machine runs through this one connection: exec,
file transfer, the forwarded Docker socket, and, on a service host, the cgroup
and quota writes. That is not incidental. The link that runs a host's containers
is the link that writes its limits, so the two can never end up pointed at
different machines.

### Enrolling

An administrator calls `POST /api/admin/hosts/{id}/agent` (or uses the admin
UI), which returns a copy-pasteable command carrying a single-use token good for
one hour.

For a machine a **user** owns:

```sh
curl -fsSL https://api.faber.example.com/api/agent/install.sh | sh -s -- --token <token>
```

For a **service host**:

```sh
curl -fsSL https://api.faber.example.com/api/agent/install.sh \
  | sudo sh -s -- --system --token <token>
```

The difference is not cosmetic. A user install writes a `systemctl --user` unit
under that account's authority and keeps its identity in
`$XDG_CONFIG_HOME/faber-agent`. A system install writes
`/etc/systemd/system/faber-agent.service`, `/usr/local/bin/faber-agent`, and
`/etc/faber-agent/config.json` (mode `0600`, holding the daemon's SSH host key
and connection credential).

**Privilege is fixed at install and there is no protocol for changing it.** A
daemon a user installed on their own machine is *physically incapable* of
writing a cgroup limit or a project quota. That is why "Faber enforces nothing
on a host a user registered" is a property of the operating system rather than
of a code path — and it is why the `--system` flag appears only in the command
Faber hands an administrator.

### Credentials

The agent's credential is equivalent to root on its machine: whoever holds it
can displace the running daemon and receive that host's launches. Treat it
accordingly.

`DELETE /api/admin/hosts/{id}/agent` revokes it. That drops the connection and
leaves every tenant, reservation, and directory untouched — reinstalling with a
fresh token restores service.

The config file *is* the identity. There is no server-side copy to reconcile
against; losing the file means reinstalling, not resyncing.

### The agent binary is x86_64 only

`GET /api/agent/binary/{arch}` serves one file per architecture from
`FABER_AGENT_BINARY_DIR`, named for what `uname -m` reports on the target. The
standard Dockerfile builds **only `faber-agent-x86_64`**.

**An arm64 machine cannot enroll today.** The installer will fail to fetch a
binary for its architecture. If you need one, add an `aarch64-unknown-linux-musl`
stage to `crates/api/Dockerfile` mirroring the existing musl stage and copy the
result in beside the x86_64 file.

In a development checkout, `FABER_AGENT_BINARY_DIR` defaults to `target/release`
and the server will serve a locally built binary for its own architecture, so
`cargo build --release -p faber-agent` is enough to make enrollment work under
`cargo run`.

---

## 6. Service hosts

A **service host** is a machine Faber operates rather than a user. Many users
share it, each one boxed in by a systemd slice, an XFS project quota, and a
container count limit.

Everything in this section is preparation Faber does **not** do at runtime.
Faber writes limits onto a machine that is already prepared, and every step
below is one whose absence surfaces as a confusing launch failure rather than a
clear one. Each step therefore says what its absence looks like.

> **Read this first.** The user-namespace and inter-container-communication
> settings in §6.3 were committed recently and **have never run against a real
> daemon.** The verification block in §6.8 exists because of that. Provision
> your first host with it open.

### The dependency chain

```
cgroup v2 + delegated controllers        (§6.1)
        ↓
faber.slice with a machine-wide cap      (§6.2)
        ↓
docker daemon.json                       (§6.3)
  systemd cgroupdriver · overlay2 · userns-remap · icc=false · log rotation
        ↓
read the remap base from /etc/subuid     (§6.4)
        ↓
XFS with prjquota, twice                 (§6.5)
  the data root · /var/lib/docker
        ↓
agent installed as --system              (§6.6)
        ↓
the host row, carrying container_root_uid (§6.7)
        ↓
verification                             (§6.8)
```

### 6.1 Kernel and cgroups

Faber puts each user's containers under `faber-<subject>.slice`, which systemd
nests under `faber.slice` automatically because of the dash. That hierarchy is
where the per-user aggregate is enforced, so the controllers must be delegated
all the way down to it.

```sh
# cgroup v2, unified hierarchy. A v1 or hybrid machine cannot serve.
mount | grep cgroup2

cat /sys/fs/cgroup/cgroup.subtree_control     # expect: cpuset cpu io memory pids
```

If `cpu`, `memory`, or `pids` is missing:

```sh
echo '+cpu +memory +pids' > /sys/fs/cgroup/cgroup.subtree_control
```

Make it survive a reboot through your distribution's usual mechanism.

Docker also needs delegation, in a drop-in for its unit:

```ini
# /etc/systemd/system/docker.service.d/delegate.conf
[Service]
Delegate=yes
```

> **If this is missing:** the limit files simply do not exist for the subtree.
> Nothing errors until Faber writes one, and then it errors at launch time with
> no indication that the cause is a controller that was never enabled. This is
> the most common silent failure on a new host.

### 6.2 The tenant slice

Created once, at provisioning, never by Faber. It carries the machine-wide cap
that makes tenant safety a kernel property rather than a property of Faber's
admission control being bug-free. If admission control is wrong, the host still
survives.

```ini
# /etc/systemd/system/faber.slice
[Unit]
Description=Faber tenants
Before=slices.target

[Slice]
# Physical memory minus a system reserve. Pick the reserve for the machine;
# everything the OS and the daemons need has to fit outside this number.
MemoryMax=56G
MemoryAccounting=yes
CPUAccounting=yes
TasksAccounting=yes
# Below system.slice's default of 100, so the OS wins under contention.
CPUWeight=50
```

```sh
systemctl daemon-reload
systemctl start faber.slice
systemctl show faber.slice -p MemoryMax   # confirm it took
```

> **If this is missing:** per-user slices still work and the machine-wide bound
> is simply absent. Faber does not create it and does not complain. One
> misjudged set of grants can then take the machine down.

### 6.3 Docker

```json
// /etc/docker/daemon.json
{
  "exec-opts": ["native.cgroupdriver=systemd"],
  "storage-driver": "overlay2",
  "userns-remap": "default",
  "icc": false,
  "log-driver": "json-file",
  "log-opts": { "max-size": "50m", "max-file": "3" }
}
```

Each line is load-bearing.

**`native.cgroupdriver=systemd`** — without it Docker writes cgroups directly
and `--cgroup-parent=faber-500001.slice` does not land where systemd thinks it
does, so every limit is written to a cgroup nothing runs in.

**`userns-remap`** is what lets a tenant be root. Without it, root in a
container is root on this machine, and Faber would have to compensate by pinning
every container to an unprivileged uid on a read-only root filesystem — which is
exactly what refuses a tenant `apt-get install`. With it, container uid 0 is a
mapped, unprivileged uid out here, so root inside is real root over the
container and nothing outside it.

The cost is stated plainly because it is the whole of the trade: **the mapping
is daemon-wide, so every tenant's container-root is the same host uid.** Docker
offers one subuid range per daemon and per-container opt-*out* only; there is no
opt-in. Tenants are kept apart by mount namespaces and by no tenant's directory
being mounted into another's container — not, any more, by owning distinct host
uids. A container escape reaches every tenant's tree rather than one. That was
weighed against a microVM per container and a daemon per tenant, both of which
keep the distinction and cost more memory per container than a small tenant node
can spare.

**`icc: false`** closes a separate hole. Faber sets no network configuration on
a container, so every container joins the default bridge — and with
inter-container communication on, one tenant's container can reach another's
over IP. This flag stops that while leaving egress alone. It applies to the
default bridge, which is where every container Faber creates lands.

**Log rotation is not optional.** Container logs land outside the project quota,
so an unrotated chatty container fills the disk without ever exceeding its
storage grant.

```sh
systemctl daemon-reload && systemctl restart docker
docker info | grep -i 'cgroup\|storage driver\|userns'
```

> **Restarting into `userns-remap` for the first time relocates existing image
> and container layers** under a subdirectory of `/var/lib/docker`, so anything
> already there appears to vanish. On a host being provisioned there is nothing
> to lose. On one that has been serving, that is a migration and not a flag flip.

### 6.4 The remap base

Faber has to be told what the daemon maps container root to:

```sh
grep dockremap /etc/subuid     # dockremap:231072:65536
```

The **first number** is the host uid container uid 0 maps to. It goes into the
host row's `container_root_uid` (§6.7), and Faber chowns each tenant's directory
to it.

Faber never reads this file. Everything Faber does to a machine comes from
configuration handed to it, never from the machine's own idea of itself — the
same principle that forbids falling back to an ambient `DOCKER_HOST`. Sniffing
`/etc/subuid` would additionally pin correctness to Docker's default remap
username.

> **If the recorded number disagrees with the daemon:** tenants get directories
> owned by a uid their container-root is not, and every container starts
> successfully and cannot write its workspace. §6.8 checks for exactly this.

### 6.5 Filesystem

A tenant's **aggregate** storage is enforced by XFS project quotas, never by
`--storage-opt`: that caps a container's upper layer only, is ignored by volumes
entirely, and gives N containers N × limit rather than the sum that was asked
for.

Both mechanisms are nonetheless required, because they bound different things.
The read-only root filesystem that used to keep the writable layer out of play
went away with the pinned uid — a tenant with real root writes to it constantly,
and it sits outside the bind mount and therefore outside the project quota. The
per-container layer cap is the only thing that sees those writes.

```sh
# ftype=1 is required for overlay2 on XFS.
xfs_info /srv/faber | grep ftype
```

Mount the data root with project quotas on:

```
# /etc/fstab
/dev/nvme0n1p2  /srv/faber       xfs  defaults,prjquota  0 0
```

**`/var/lib/docker` needs `prjquota` too — not conditionally.** `--storage-opt`
is how the per-container layer cap is applied, and on XFS it needs project
quotas on the filesystem holding the layers.

```sh
mount -o remount /srv/faber
xfs_quota -x -c 'state' /srv/faber   # project quota should read ON
```

Faber creates `{user_data_root}/{subject}/work` and `.../scratch` itself, sets
the project id, and chowns them to `container_root_uid` — through the agent, so
"writable" means writable by root on this machine. **The data root itself must
exist before the first launch.**

The owner is the remapped container root, not the subject id, because the tenant
is root inside their container and has to be able to write the workspace they
are given. The subject id remains the project id and still names the systemd
slice; it has simply stopped being a uid, so a file's owner out here no longer
says which tenant wrote it. The path and the project id do.

> **If `/var/lib/docker` lacks `prjquota`:** the per-container cap is silently
> absent — not an error — and a tenant's writable layer is unbounded. Since that
> cap is now the only bound on that path, this is a disk-fills-up failure with no
> warning.

### 6.6 The agent

`systemctl set-property --runtime`, `xfs_quota -x`, `chown`, and `chmod` all run
**on this machine by the Faber agent installed on it**, not by the API process.
The API is a container somewhere else; it has no `systemctl`, no `xfs_quota`,
and a `/sys/fs/cgroup` that resolves to its own subtree.

So the agent must be a **system** install running as root:

```sh
curl -fsSL https://api.faber.example.com/api/agent/install.sh \
  | sudo sh -s -- --system --token <token>
```

See [§5](#5-agents) for what that writes and how to revoke it.

> **If it is installed as a user service:** it connects, it looks healthy, and
> it cannot write a single limit. This failure mode is by design — it is what
> makes "Faber enforces nothing on a user's own machine" true at the operating
> system level — but on a service host it is a misconfiguration that presents as
> a working host.

### 6.7 The host row

`POST /api/admin/hosts`, as an administrator. The equivalent insert, for
reading:

```sql
INSERT INTO host (id, user_id, name, transport, exec_mode, docker_endpoint,
                  user_data_root, container_root_uid,
                  default_cpu_millis, default_memory_bytes,
                  default_storage_bytes, default_container_max)
VALUES (gen_random_uuid(), NULL, 'shared-1', 'agent', 'docker',
        'unix:///var/run/docker.sock', '/srv/faber', 231072,
        2000, 4294967296, 53687091200, 3);
```

- **`user_id IS NULL` is the entire marker of a service host.** There is no
  `kind` column and no flag, because a flag that can disagree with ownership is
  a bug surface.
- **`docker_endpoint`** names the socket on *this machine's* filesystem, reached
  over the agent's connection with a forwarded channel. It must be a unix socket
  path; `tcp://` is refused.
- **`container_root_uid`** is the number from §6.4. A CHECK constraint requires
  it on a service host, for the same reason one requires `user_data_root`:
  without it there is no correct owner for a tenant directory. The API refuses
  `0` specifically — that is not a mistyped uid, it is host root, and it would
  pass the CHECK.
- **The four `default_*` columns are the per-user ceiling, and NULL means
  unlimited** — not "inherit". Leaving one out grants that resource without
  limit.

The row is half the registration. A host created here is unreachable until the
agent install has been run: `agent.connected` is false, the storage section reads
null, and launches fail as unreachable rather than silently landing somewhere.

**Service images** are inserted the same way, with `user_id = NULL`. A service
host accepts only service images, and the reference must be pulled before the
first launch or the first launch pays for it.

One thing that interacts with the user-namespace change: **the container is
pinned to `0:0`**, so a service image's `USER` directive is overridden. That is
deliberate — a service host exists to give a tenant root, and leaving the uid to
the image would make that true only for images whose Dockerfile happens not to
set `USER`.

### 6.8 Verification

All of these run **on the host**, not where the API runs.

```sh
# Is the agent up and dialed in?
systemctl status faber-agent

# A user slice appears once their first container starts.
systemctl show faber-500001.slice -p MemoryMax -p CPUQuotaPerSecUSec -p TasksMax
cat /sys/fs/cgroup/faber.slice/faber-500001.slice/memory.max

# Their project quota.
xfs_quota -x -c 'quota -p -N -b -v 500001' /srv/faber

# The remap is on, and the number Faber was given is the one in force.
docker info --format '{{.SecurityOptions}}' | grep -q userns && echo remapped
grep dockremap /etc/subuid
stat -c '%u %a' /srv/faber/500001

# Inter-container communication is off.
docker network inspect bridge \
  --format '{{index .Options "com.docker.network.bridge.enable_icc"}}'   # false
```

The last four are the ones that fail quietly, and they are the whole of what
separates a working host from one that looks working. `stat` should print the
same uid as `/etc/subuid`'s second field, and `700`. A tenant directory owned by
anything else means the host row's `container_root_uid` disagrees with the
daemon, and the symptom will be a container that starts fine and cannot write.

Two more that Faber cannot make for you, because both are properties of the
machine rather than of a row:

```sh
# Root inside a container is real root there, and nobody out here.
docker run --rm debian:stable-slim \
  sh -c 'id -u; apt-get update -qq && echo apt-ok'

# One tenant's container cannot reach another's. Both land on the default
# bridge, which is what icc=false covers.
docker run -d --name icc-a alpine sleep 60
docker run -d --name icc-b alpine sleep 60
B=$(docker inspect -f '{{.NetworkSettings.IPAddress}}' icc-b)
docker exec icc-a ping -c1 -W1 "$B" && echo 'REACHABLE — icc is not off'
docker rm -f icc-a icc-b
```

The first must print `0` and then `apt-ok`. The second must **fail** the ping;
printing `REACHABLE` means one tenant can reach another's containers over IP and
the host is not fit to serve.

The `bound_environments` tool reports the same numbers back from inside a run, read live on
every call, which is the fastest way to confirm the whole chain end to end.

### 6.9 Releasing a tenant is destructive, immediately

`DELETE /api/admin/hosts/{id}/users/{user_id}` — and the equivalent action a
user can take on themselves — **sets the project quota to zero and `rm -rf`s the
tenant's directory in the same call that tombstones the row.**

There is no retention window and no export. Neither is expressible: both would
need somewhere for the bytes to go and a sweeper to eventually forget them, and
neither exists. Both routes that reach it say so before they ask.

It is refused while the tenant still holds containers, since the reservation is
what their directory sits inside.

### 6.10 Numbers this document does not decide

The four `default_*` values are a deployment decision. Four more are chosen in
code and are currently reasoned guesses:

- **`DEFAULT_PIDS_MAX`** (4096) — generous enough for a parallel build, tight
  enough to stop a fork bomb.
- **`CONTAINER_LAYER_BYTES`** (10 GiB) — the per-container writable-layer cap.
  The bound that matters is `container_max × this` against the filesystem
  reserve. This stopped being a secondary bound when the read-only root went
  away: it is now the only thing between a tenant with root and an unbounded
  write path the project quota cannot see. 10 GiB was chosen when it was a
  backstop and deserves re-choosing.
- **`STORAGE_RESERVE_RATIO`** (10%) and **`MEMORY_FLOOR_RATIO`** (15%).

### 6.11 Known gaps

- **Disk IOPS is unlimited.** `io.max` on the user slice is the obvious lever,
  but the values are device-dependent and a badly chosen one is worse than none.
  A user can currently thrash the disk for everyone. Highest-priority gap.
- **Network egress is unlimited.**
- **Expiry lag.** A temporary grant is ignored by the read path the instant it
  lapses, so nobody gains by a sweep being late — but the *machine* keeps
  honouring the old limit until the next sweep, up to 60 seconds.
- **Only x86_64 hosts can enroll**, per [§5](#the-agent-binary-is-x86_64-only).

---

## 7. Bootstrapping an administrator

**Nothing in the API can make the first administrator.** There is no bootstrap
route, no seed user, and no environment variable. Without this step no service
host can be created at all, so do it before §6.

The `users.admin_since` column is NULL for "not an administrator" and set for
"is one":

```sql
-- Promote
UPDATE users SET admin_since = now() WHERE id = '<user uuid>';

-- Demote
UPDATE users SET admin_since = NULL WHERE id = '<user uuid>';
```

The user must exist first, which means they must have signed in through Surge at
least once. Find them by whatever identifier your Surge deployment uses, then
promote by UUID.

---

## 8. Operating

### Deploy order

1. Postgres reachable.
2. Start the API. Migrations apply automatically; a failure is a container that
   will not come up.
3. Start the UI.
4. Promote an administrator (§7).
5. Prepare and register service hosts (§6).

### Upgrading a deployment that already has a service host

**Read this before rolling the image if you have a service host provisioned.**

The user-namespace change is not backward compatible with a host prepared
before it. A host that was serving under the old model has a docker daemon
without `userns-remap` and a `host` row without `container_root_uid`, and the
new code needs both. Nothing about the upgrade repairs that automatically,
because the value it would need — the first subuid of that machine's remap user
— is a fact about a machine the database has never seen. Guessing it would
produce tenant directories owned by a uid the containers do not run as, so every
container would start successfully and be unable to write.

So the migration adds its constraint `NOT VALID`: it binds every row written
from then on and says nothing about rows that predate it. **The migration
applies cleanly against an existing service host, and that host's launches are
then refused** with an error naming the fix, until you do this:

1. **Prepare the daemon** on that machine — add `"userns-remap": "default"` and
   `"icc": false` to `/etc/docker/daemon.json` and restart docker, per
   [§6.3](#63-docker). Restarting into `userns-remap` relocates existing image
   and container layers under a subdirectory of `/var/lib/docker`; images will
   need re-pulling.

2. **Confirm `/var/lib/docker` has `prjquota`** ([§6.5](#65-filesystem)). It was
   conditional before and is not now.

3. **Read the remap base and record it:**

   ```sh
   grep dockremap /etc/subuid       # on the host
   ```

   ```sql
   -- against Faber's database
   UPDATE host SET container_root_uid = 231072 WHERE user_id IS NULL AND name = 'shared-1';
   ```

   Or `PATCH /api/admin/hosts/{id}` with `{"container_root_uid": 231072}`.

4. **Existing tenant directories are re-owned on the next launch.** Faber's
   `chown -R` is idempotent and runs on every launch, so no manual `chown` is
   needed — but the first launch after the change walks each tenant's whole
   tree, which is slow in proportion to its size.

5. **Verify** with [§6.8](#68-verification), then close the constraint:

   ```sql
   ALTER TABLE host VALIDATE CONSTRAINT host_service_needs_container_root_uid;
   ```

   This takes no exclusive lock and is safe against a live deployment. After it,
   a service host without the column is rejected by the database rather than at
   launch.

One behavioural change to expect on that host regardless: containers now run as
**root** (`0:0`), overriding any `USER` in a service image, and the root
filesystem is no longer read-only. Both are the point of the change, but an
image that assumed either will behave differently.

### Ordinary upgrades

The API applies pending migrations at boot, so an upgrade is a normal image
roll — with the single-process caveat from §2 about concurrent starts.

Because the agent binary is built into the API image and served from it, an API
upgrade changes the binary new hosts receive. **Existing agents are not
upgraded**; they keep running the binary they installed. Re-running the install
command with a fresh enrollment token is how an agent is updated.

### What to back up

- **Postgres.** Everything durable is here.
- **`FABER_MASTER_KEY`.** Not in the database, and its loss is unrecoverable in
  the sense that matters — see §3.
- **Each service host's `/srv/faber`** (or whatever `user_data_root` is), if
  tenant workspaces are worth keeping. Faber has no backup mechanism and no
  export path.

Agent config files (`/etc/faber-agent/config.json`) are worth backing up only if
re-enrolling is inconvenient; re-running the installer with a fresh token is the
supported recovery.

### Logs

The API logs through `tracing` to stdout with an HTTP trace layer. There is no
log file and no rotation to configure — that is your runtime's job.

`RUST_LOG` sets the filter. **The default when it is unset is
`api=debug,tower_http=debug`**, which is a development default and is noisy in
production. Set it explicitly:

```sh
RUST_LOG=api=info,tower_http=warn
```

### `.env` files

The API calls `dotenvy::dotenv()` before reading anything, so a `.env` file in
the working directory is loaded if present. That is convenient in a checkout and
a hazard in an image — a stray `.env` baked into a container silently overrides
nothing (real environment variables win) but a *missing* one can make a locally
working configuration look like a deployment bug. Prefer real environment
variables in production and treat `.env` as a development affordance.

---

## 9. Things that are not deployed

Worth stating so you don't go looking:

- **`crates/traefik`** is a library for expressing domain → container routing as
  Traefik dynamic configuration. **It has no caller.** Nothing in the API
  references it, so there is no Traefik integration to configure today.
- **`crates/search`, `crates/readable`, `crates/llm`, `crates/harness`,
  `crates/environment`** are libraries linked into the API, not separate
  services.
- There is **no message queue, no cache, no object store.** Postgres and the
  filesystem are the whole of the state.
- There is **no metrics endpoint.** `/health` is liveness only.
