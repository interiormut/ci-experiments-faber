import * as React from "react"
import { createFileRoute } from "@tanstack/react-router"
import {
  Box,
  ChevronDown,
  HardDrive,
  Pencil,
  PlugZap,
  Plus,
  Power,
  Server,
  ShieldCheck,
  SlidersHorizontal,
  Trash2,
  UserMinus,
} from "lucide-react"

import { FaberError, type ServiceHost, type ServiceImage, type Tenant } from "@/lib/api"
import { useServiceHosts } from "@/lib/admin/use-service-hosts"
import { bytes, cores, count, summary } from "@/lib/hosts/limits"
import { committedFraction, usedFraction } from "@/lib/admin/limits"
import { age } from "@/lib/hosts/labels"
import { useAppShell } from "@/components/shell/app-shell"
import { Button } from "@/components/ui/button"
import {
  GrantDialog,
  ServiceHostAgentDialog,
  ServiceHostFormDialog,
  ServiceImageFormDialog,
} from "@/components/admin/service-host-dialogs"
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog"

/**
 * The machines faber provides, and who is on them.
 *
 * Everything on this page is somebody else's environment. A service host is
 * shared, so the two questions it has to answer are the two a user's own host
 * never raises: how much of the machine any single tenant may take, and how
 * much of it is already promised.
 *
 * Storage is shown against the *ceiling* rather than against the disk. It is
 * the one resource faber refuses to overcommit — running out of it is global
 * and cannot be repaired by whoever caused it — so the number that matters is
 * how much is left to promise, not how much is left on the volume.
 *
 * Usage is read from the machine on every request and stored nowhere. Blanks
 * mean the machine did not answer, never that a tenant is using nothing.
 */
export const Route = createFileRoute("/admin")({ component: AdminPage })

function AdminPage() {
  const { me } = useAppShell()
  const {
    hosts,
    images,
    tenants,
    loaded,
    error,
    loadTenants,
    addHost,
    editHost,
    removeHost,
    reloadHost,
    revokeAgent,
    grant,
    revoke,
    release,
    addImage,
    editImage,
    removeImage,
  } = useServiceHosts()

  const [hostDialogOpen, setHostDialogOpen] = React.useState(false)
  const [editingHost, setEditingHost] = React.useState<ServiceHost | null>(null)
  const [deleteTarget, setDeleteTarget] = React.useState<ServiceHost | null>(null)
  const [deleting, setDeleting] = React.useState(false)
  const [deleteError, setDeleteError] = React.useState<string | null>(null)

  const [grantTarget, setGrantTarget] = React.useState<
    { host: ServiceHost; tenant: Tenant } | null
  >(null)

  const [releaseTarget, setReleaseTarget] = React.useState<
    { host: ServiceHost; tenant: Tenant } | null
  >(null)
  const [releasing, setReleasing] = React.useState(false)
  const [releaseError, setReleaseError] = React.useState<string | null>(null)

  const [agentTarget, setAgentTarget] = React.useState<
    { host: ServiceHost; fresh: boolean } | null
  >(null)
  // A host that was just registered, waiting for the form dialog to close
  // before the install flow opens over it. Two dialogs open at once fight
  // over focus, so they take turns.
  const [pendingAgent, setPendingAgent] = React.useState<ServiceHost | null>(null)

  React.useEffect(() => {
    if (hostDialogOpen || !pendingAgent) return
    setAgentTarget({ host: pendingAgent, fresh: true })
    setPendingAgent(null)
  }, [hostDialogOpen, pendingAgent])

  const [imageDialogOpen, setImageDialogOpen] = React.useState(false)
  const [editingImage, setEditingImage] = React.useState<ServiceImage | null>(null)
  const [deleteImageTarget, setDeleteImageTarget] = React.useState<ServiceImage | null>(null)

  // Bumped on every open so a dialog remounts with a fresh draft, the same way
  // the other management pages do it.
  const [formKey, setFormKey] = React.useState(0)
  const bump = () => setFormKey((k) => k + 1)

  const openCreateHost = () => {
    setEditingHost(null)
    bump()
    setHostDialogOpen(true)
  }

  const openEditHost = (host: ServiceHost) => {
    setEditingHost(host)
    bump()
    setHostDialogOpen(true)
  }

  const handleDelete = async () => {
    if (!deleteTarget) return
    setDeleting(true)
    setDeleteError(null)
    try {
      await removeHost(deleteTarget.id)
      setDeleteTarget(null)
    } catch (err) {
      // Shown, not just kept: the common refusal here names how many people
      // are materialised on the host, and a Delete button that quietly does
      // nothing is indistinguishable from one that is broken.
      setDeleteError(
        err instanceof FaberError ? err.message : "could not delete this host",
      )
    } finally {
      setDeleting(false)
    }
  }

  const handleRelease = async () => {
    if (!releaseTarget) return
    setReleasing(true)
    setReleaseError(null)
    try {
      await release(releaseTarget.host.id, releaseTarget.tenant.user_id)
      setReleaseTarget(null)
    } catch (err) {
      // The two refusals here are both actionable and both invisible
      // otherwise: containers still registered, and a machine that is not
      // answering. Neither is something to retry blindly.
      setReleaseError(
        err instanceof FaberError ? err.message : "could not release this tenant",
      )
    } finally {
      setReleasing(false)
    }
  }

  // The routes refuse on their own, so this is only about not showing an
  // operator's page to someone who reached the URL anyway.
  if (me && !me.admin) {
    return (
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto flex w-full max-w-2xl flex-col items-center gap-3 px-4 py-24 text-center">
          <ShieldCheck className="h-6 w-6 text-muted-foreground" />
          <p className="text-sm font-medium">Not your machines</p>
          <p className="text-sm text-muted-foreground">
            This account does not administer the hosts faber provides.
          </p>
        </div>
      </div>
    )
  }

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto flex w-full max-w-2xl flex-col gap-10 px-4 py-10">
        <section className="flex flex-col gap-6">
          <div className="flex items-center justify-between gap-4">
            <div>
              <h1 className="text-lg font-semibold tracking-tight">Service hosts</h1>
              <p className="text-sm text-muted-foreground">
                The machines faber operates. Many users share one, so each gets
                a ceiling rather than the machine.
              </p>
            </div>
            <Button size="sm" onClick={openCreateHost}>
              <Plus className="h-4 w-4" />
              Add host
            </Button>
          </div>

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          {!loaded ? (
            <p className="text-sm text-muted-foreground">Loading…</p>
          ) : hosts.length === 0 ? (
            <div className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border py-16 text-center">
              <Server className="h-6 w-6 text-muted-foreground" />
              <div>
                <p className="text-sm font-medium">No service hosts</p>
                <p className="text-sm text-muted-foreground">
                  Users can only run on machines of their own until faber
                  provides one.
                </p>
              </div>
              <Button size="sm" onClick={openCreateHost}>
                <Plus className="h-4 w-4" />
                Add host
              </Button>
            </div>
          ) : (
            <ul className="flex flex-col gap-3">
              {hosts.map((host) => (
                <HostCard
                  key={host.id}
                  host={host}
                  tenants={tenants[host.id]}
                  onLoadTenants={() => loadTenants(host.id)}
                  onEdit={() => openEditHost(host)}
                  onAgent={() => setAgentTarget({ host, fresh: false })}
                  onDelete={() => setDeleteTarget(host)}
                  onToggleDisabled={() =>
                    editHost(host.id, { disabled: !host.disabled_at }).catch(() => {
                      // The row keeps showing the server's last answer.
                    })
                  }
                  onGrant={(tenant) => setGrantTarget({ host, tenant })}
                  onRelease={(tenant) => setReleaseTarget({ host, tenant })}
                />
              ))}
            </ul>
          )}
        </section>

        <section className="flex flex-col gap-4">
          <div className="flex items-center justify-between gap-4">
            <div>
              <h2 className="text-sm font-semibold tracking-tight">Service images</h2>
              <p className="text-sm text-muted-foreground">
                The only templates a service host will start. A shared host with
                none is a machine nobody can spawn on.
              </p>
            </div>
            <Button
              size="sm"
              variant="outline"
              onClick={() => {
                setEditingImage(null)
                bump()
                setImageDialogOpen(true)
              }}
            >
              <Plus className="h-4 w-4" />
              Add image
            </Button>
          </div>

          {images.length === 0 ? (
            <p className="rounded-xl border border-dashed border-border px-4 py-8 text-center text-sm text-muted-foreground">
              No service images yet.
            </p>
          ) : (
            <ul className="flex flex-col gap-2">
              {images.map((image) => (
                <li
                  key={image.id}
                  className="flex items-start justify-between gap-4 rounded-xl border border-border bg-card px-4 py-3"
                >
                  <div className="min-w-0">
                    <div className="flex items-center gap-2">
                      <Box className="h-4 w-4 shrink-0 text-muted-foreground" />
                      <span className="truncate text-sm font-medium">{image.name}</span>
                    </div>
                    <p className="truncate text-xs text-muted-foreground">
                      {image.reference} · {image.default_root_path}
                    </p>
                  </div>
                  <div className="flex shrink-0 items-center gap-1">
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      aria-label={`Edit ${image.name}`}
                      onClick={() => {
                        setEditingImage(image)
                        bump()
                        setImageDialogOpen(true)
                      }}
                    >
                      <Pencil className="h-4 w-4" />
                    </Button>
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      aria-label={`Delete ${image.name}`}
                      onClick={() => setDeleteImageTarget(image)}
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </section>
      </div>

      <ServiceHostFormDialog
        key={`service-host-${formKey}`}
        open={hostDialogOpen}
        onOpenChange={setHostDialogOpen}
        editing={editingHost}
        onCreate={async (body) => {
          const created = await addHost(body)
          // Straight into the install flow, once this dialog is out of the
          // way. The row alone is not a usable machine — faber cannot reach
          // it until a daemon dials in — and an operator who stops here has
          // registered a host that can do nothing.
          setPendingAgent(created)
          return created
        }}
        onUpdate={editHost}
      />

      <ServiceHostAgentDialog
        key={`agent-${agentTarget?.host.id ?? "none"}`}
        open={!!agentTarget}
        onOpenChange={(open) => !open && setAgentTarget(null)}
        host={agentTarget?.host ?? null}
        fresh={agentTarget?.fresh}
        onConnected={(id) => {
          void reloadHost(id)
        }}
        onRevoke={revokeAgent}
      />

      <GrantDialog
        key={`grant-${grantTarget?.tenant.user_id ?? "none"}-${formKey}`}
        open={!!grantTarget}
        onOpenChange={(open) => !open && setGrantTarget(null)}
        host={grantTarget?.host ?? null}
        tenant={grantTarget?.tenant ?? null}
        onGrant={grant}
        onRevoke={revoke}
      />

      <ServiceImageFormDialog
        key={`service-image-${formKey}`}
        open={imageDialogOpen}
        onOpenChange={setImageDialogOpen}
        editing={editingImage}
        onCreate={addImage}
        onUpdate={editImage}
      />

      <AlertDialog
        open={!!deleteTarget}
        onOpenChange={(open) => {
          if (open) return
          setDeleteTarget(null)
          setDeleteError(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete {deleteTarget?.name}?</AlertDialogTitle>
            <AlertDialogDescription>
              This drops faber&apos;s registration of the machine. Nothing on the
              machine itself is touched — no tenant directory, no container. It
              is refused while anyone is materialised there; disabling the host
              instead stops new launches and leaves what is running alone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          {deleteError ? (
            <p className="text-sm text-destructive">{deleteError}</p>
          ) : null}
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deleting}>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={(event) => {
                event.preventDefault()
                void handleDelete()
              }}
              disabled={deleting}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              {deleting ? "Deleting…" : "Delete"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={!!releaseTarget}
        onOpenChange={(open) => {
          if (open) return
          setReleaseTarget(null)
          setReleaseError(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              Release tenant {releaseTarget?.tenant.subject_id ?? "—"} from{" "}
              {releaseTarget?.host.name}?
            </AlertDialogTitle>
            <AlertDialogDescription>
              This deletes their directory on that machine — everything they
              have there, right now. There is no retention window and no export,
              and nothing brings it back. What it returns is their storage
              reservation, which the host can then promise to somebody else.
              Their limits here survive, in case they ever come back.
            </AlertDialogDescription>
          </AlertDialogHeader>
          {releaseError ? (
            <p className="text-sm text-destructive">{releaseError}</p>
          ) : null}
          <AlertDialogFooter>
            <AlertDialogCancel disabled={releasing}>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={(event) => {
                event.preventDefault()
                void handleRelease()
              }}
              disabled={releasing}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              {releasing ? "Releasing…" : "Delete their data"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={!!deleteImageTarget}
        onOpenChange={(open) => !open && setDeleteImageTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete {deleteImageTarget?.name}?</AlertDialogTitle>
            <AlertDialogDescription>
              Containers already started from it keep running — the link back to
              a template is provenance and nothing resolves through it. Nobody
              will be able to start a new one.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={(event) => {
                event.preventDefault()
                if (!deleteImageTarget) return
                void removeImage(deleteImageTarget.id)
                  .then(() => setDeleteImageTarget(null))
                  .catch(() => {})
              }}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              Delete
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

function Badge({ children }: { children: React.ReactNode }) {
  return (
    <span className="shrink-0 rounded-full bg-muted px-2 py-0.5 text-[11px] font-medium text-muted-foreground">
      {children}
    </span>
  )
}

/** A proportion as a bar. Never drawn from a guess — the caller passes `null`
 *  when either half of the fraction is missing. */
function Meter({ fraction }: { fraction: number | null }) {
  if (fraction === null) return null
  return (
    <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
      <div
        className="h-full rounded-full bg-foreground/40"
        style={{ width: `${Math.round(fraction * 100)}%` }}
      />
    </div>
  )
}

function HostCard({
  host,
  tenants,
  onLoadTenants,
  onEdit,
  onAgent,
  onDelete,
  onToggleDisabled,
  onGrant,
  onRelease,
}: {
  host: ServiceHost
  tenants: Tenant[] | undefined
  onLoadTenants: () => Promise<Tenant[]>
  onEdit: () => void
  onAgent: () => void
  onDelete: () => void
  onToggleDisabled: () => void
  onGrant: (tenant: Tenant) => void
  onRelease: (tenant: Tenant) => void
}) {
  const [open, setOpen] = React.useState(false)
  const [loading, setLoading] = React.useState(false)
  const draining = !!host.disabled_at
  // Three states, not two, and they want different sentences. A host nobody
  // has installed a daemon on was never finished; one that enrolled and is
  // now silent is a machine that has gone away.
  const daemon = host.agent.connected
    ? "connected"
    : host.agent.enrolled_at
      ? "offline"
      : "absent"

  const toggle = () => {
    const next = !open
    setOpen(next)
    // Fetched on first expand: each row carries a live read of the machine, so
    // asking for every host's tenants up front would read the filesystem once
    // per user before anything is on screen.
    if (next && !tenants && !loading) {
      setLoading(true)
      void onLoadTenants().finally(() => setLoading(false))
    }
  }

  return (
    <li className="rounded-xl border border-border bg-card">
      <div className="flex items-start justify-between gap-4 px-4 py-3">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="truncate text-sm font-medium">{host.name}</span>
            {draining ? <Badge>draining</Badge> : null}
            {daemon === "connected" ? null : (
              <Badge>{daemon === "absent" ? "no daemon" : "daemon offline"}</Badge>
            )}
            <Badge>
              {host.tenants} {host.tenants === 1 ? "tenant" : "tenants"}
            </Badge>
          </div>
          <p className="truncate text-xs text-muted-foreground">
            {host.docker_endpoint ?? "local socket"} · {host.user_data_root ?? "no data root"}
          </p>
          <p className="mt-1 text-xs text-muted-foreground">
            Default per user: {summary(host.defaults)}
          </p>

          {host.storage ? (
            <div className="mt-3 flex flex-col gap-1">
              <Meter fraction={committedFraction(host.storage)} />
              <p className="text-xs text-muted-foreground">
                <HardDrive className="mr-1 inline h-3 w-3" />
                {bytes(host.storage.committed_bytes)} promised of{" "}
                {bytes(host.storage.ceiling_bytes)} promisable ·{" "}
                {bytes(host.storage.available_bytes)} free on disk
              </p>
            </div>
          ) : daemon === "absent" ? (
            <div className="mt-3 flex flex-col items-start gap-2">
              <p className="text-xs text-muted-foreground">
                Registered, but nothing can run here yet: faber reaches this
                machine through a daemon installed on it, and there is none.
              </p>
              <Button size="sm" variant="outline" onClick={onAgent}>
                <PlugZap className="h-4 w-4" />
                Install daemon
              </Button>
            </div>
          ) : (
            <p className="mt-3 text-xs text-muted-foreground">
              The daemon is not connected, so faber cannot read this machine.
              It dials in on its own — if it stays away, the machine or its
              service is down.
            </p>
          )}
        </div>

        <div className="flex shrink-0 items-center gap-1">
          <Button
            size="icon-sm"
            variant="ghost"
            aria-label={draining ? `Resume ${host.name}` : `Drain ${host.name}`}
            title={draining ? "Resume" : "Drain"}
            onClick={onToggleDisabled}
          >
            <Power className="h-4 w-4" />
          </Button>
          <Button
            size="icon-sm"
            variant="ghost"
            aria-label={`Daemon on ${host.name}`}
            title="Daemon"
            onClick={onAgent}
          >
            <PlugZap className="h-4 w-4" />
          </Button>
          <Button size="icon-sm" variant="ghost" aria-label={`Edit ${host.name}`} onClick={onEdit}>
            <Pencil className="h-4 w-4" />
          </Button>
          <Button
            size="icon-sm"
            variant="ghost"
            aria-label={`Delete ${host.name}`}
            onClick={onDelete}
          >
            <Trash2 className="h-4 w-4" />
          </Button>
        </div>
      </div>

      <button
        type="button"
        onClick={toggle}
        aria-expanded={open}
        className="flex w-full items-center gap-1.5 border-t border-border px-4 py-2 text-left text-xs text-muted-foreground hover:text-foreground"
      >
        <ChevronDown
          className={`h-3.5 w-3.5 transition-transform ${open ? "" : "-rotate-90"}`}
        />
        Tenants
      </button>

      {open ? (
        <div className="border-t border-border px-4 py-3">
          {loading && !tenants ? (
            <p className="text-sm text-muted-foreground">Loading…</p>
          ) : !tenants || tenants.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              Nobody is materialised here yet. A tenant appears the first time
              they launch something — that is when their directory is created
              and their storage reserved.
            </p>
          ) : (
            <ul className="flex flex-col gap-3">
              {tenants.map((tenant) => (
                <TenantRow
                  key={tenant.user_id}
                  tenant={tenant}
                  onGrant={() => onGrant(tenant)}
                  onRelease={() => onRelease(tenant)}
                />
              ))}
            </ul>
          )}
        </div>
      ) : null}
    </li>
  )
}

function TenantRow({
  tenant,
  onGrant,
  onRelease,
}: {
  tenant: Tenant
  onGrant: () => void
  onRelease: () => void
}) {
  return (
    <li className="flex items-start justify-between gap-3">
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="font-mono text-xs">{tenant.subject_id ?? "—"}</span>
          {tenant.quota.granted ? <Badge>granted</Badge> : <Badge>default</Badge>}
          {tenant.quota.expires_at ? (
            <Badge>expires {age(tenant.quota.expires_at)}</Badge>
          ) : null}
        </div>
        <p className="truncate text-xs text-muted-foreground">
          {cores(tenant.quota.cpu_millis)} · {bytes(tenant.quota.memory_bytes)} RAM ·{" "}
          {tenant.containers}/{count(tenant.quota.container_max)} containers
        </p>
        <div className="mt-1.5 flex flex-col gap-1">
          <Meter fraction={usedFraction(tenant)} />
          <p className="text-xs text-muted-foreground">
            {tenant.usage.storage_bytes === null
              ? `${bytes(tenant.quota.storage_bytes)} granted · usage unread`
              : `${bytes(tenant.usage.storage_bytes)} of ${bytes(tenant.quota.storage_bytes)} used`}
          </p>
        </div>
        {tenant.quota.note ? (
          <p className="mt-1 truncate text-xs text-muted-foreground italic">
            {tenant.quota.note}
          </p>
        ) : null}
      </div>
      <div className="flex shrink-0 items-center gap-1">
        <Button size="sm" variant="ghost" onClick={onGrant}>
          <SlidersHorizontal className="h-4 w-4" />
          Limits
        </Button>
        <Button
          size="icon-sm"
          variant="ghost"
          aria-label={`Release ${tenant.subject_id ?? "this tenant"}`}
          title="Release"
          onClick={onRelease}
        >
          <UserMinus className="h-4 w-4" />
        </Button>
      </div>
    </li>
  )
}
