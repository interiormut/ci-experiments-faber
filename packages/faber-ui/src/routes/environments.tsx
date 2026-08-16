import * as React from "react"
import { createFileRoute } from "@tanstack/react-router"
import {
  Box,
  ChevronDown,
  Container,
  Eraser,
  Layers,
  Pencil,
  Plus,
  Server,
  Sparkles,
  Trash2,
  Unplug,
  Users,
} from "lucide-react"

import { FaberError, type Host, type HostContainer, type Image } from "@/lib/api"
import { useHosts } from "@/lib/hosts/use-hosts"
import { useImages } from "@/lib/hosts/use-images"
import { useHostUsage } from "@/lib/hosts/use-host-usage"
import { addressLabel, observation, toolList } from "@/lib/hosts/labels"
import { bytes, cores, count } from "@/lib/hosts/limits"
import { Button } from "@/components/ui/button"
import {
  ContainerFormDialog,
  ContainerSpawnDialog,
  ImageFormDialog,
} from "@/components/hosts/host-dialogs"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
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
 * The environments themselves: the places a run can actually land.
 *
 * `internal-docs/host.md` puts it plainly — the registration primitive is the
 * host, but the *environment* is what you bind a run to. A direct host is one
 * environment, itself; a docker host contributes one per registered container
 * and none of its own. So every host registered on `/hosts` surfaces here, and
 * this is the only page that manages containers.
 *
 * The probe is stated as an observation, never as status. There is no green dot
 * here and nothing on this page should grow one: the authoritative answer to
 * "is it reachable" is the next connection attempt, and a light would invite
 * reading a cached one instead.
 *
 * A host faber provides is listed here alongside the user's own, because what
 * they are choosing is a place to run and which of the two it is belongs in
 * the row rather than on a page of its own. It renders differently in three
 * ways, all of them consequences of the machine being shared: it carries the
 * caller's ceiling and what they are using against it, it starts containers
 * only from faber's own templates, and a container on it is deleted rather
 * than forgotten — unregistering one would leave it running on somebody
 * else's machine, out of sight and out of the count.
 */
export const Route = createFileRoute("/environments")({ component: EnvironmentsPage })

function EnvironmentsPage() {
  const {
    hosts,
    loaded,
    error,
    addContainer,
    spawnContainer,
    editContainer,
    unregisterContainer,
    releaseTenancy,
  } = useHosts()
  const images = useImages()

  const [dialogOpen, setDialogOpen] = React.useState(false)
  const [spawnOpen, setSpawnOpen] = React.useState(false)
  const [dialogHost, setDialogHost] = React.useState<Host | null>(null)
  const [editing, setEditing] = React.useState<HostContainer | null>(null)
  const [formKey, setFormKey] = React.useState(0)

  const [unregisterTarget, setUnregisterTarget] = React.useState<
    { host: Host; container: HostContainer; destroy: boolean } | null
  >(null)
  const [unregistering, setUnregistering] = React.useState(false)

  const [releaseTarget, setReleaseTarget] = React.useState<Host | null>(null)
  const [releasing, setReleasing] = React.useState(false)
  const [releaseError, setReleaseError] = React.useState<string | null>(null)
  // Bumped on a successful release so every row re-reads the machine. The
  // host list is reloaded too, but `materialised` comes from the usage route
  // and would otherwise keep offering a button for a directory that is gone.
  const [releaseNonce, setReleaseNonce] = React.useState(0)

  const openAdd = (host: Host) => {
    setDialogHost(host)
    setEditing(null)
    setFormKey((k) => k + 1)
    setDialogOpen(true)
  }

  const openCreate = (host: Host) => {
    setDialogHost(host)
    setFormKey((k) => k + 1)
    setSpawnOpen(true)
  }

  const openEdit = (host: Host, container: HostContainer) => {
    setDialogHost(host)
    setEditing(container)
    setFormKey((k) => k + 1)
    setDialogOpen(true)
  }

  const handleUnregister = async () => {
    if (!unregisterTarget) return
    setUnregistering(true)
    try {
      await unregisterContainer(
        unregisterTarget.host.id,
        unregisterTarget.container.id,
        unregisterTarget.destroy,
      )
      setUnregisterTarget(null)
    } catch {
      // The dialog stays open with the target set so the user can retry.
    } finally {
      setUnregistering(false)
    }
  }

  const handleRelease = async () => {
    if (!releaseTarget) return
    setReleasing(true)
    setReleaseError(null)
    try {
      await releaseTenancy(releaseTarget.id)
      setReleaseNonce((n) => n + 1)
      setReleaseTarget(null)
    } catch (err) {
      // Both refusals are the user's to act on — containers still registered
      // here, or a machine that is not answering right now — and neither is
      // visible unless it is said.
      setReleaseError(
        err instanceof FaberError
          ? err.message
          : "could not give back your space on this machine",
      )
    } finally {
      setReleasing(false)
    }
  }

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto flex w-full max-w-2xl flex-col gap-10 px-4 py-10">
        <section className="flex flex-col gap-6">
          <div>
            <h1 className="text-lg font-semibold tracking-tight">Environments</h1>
            <p className="text-sm text-muted-foreground">
              Where a run can land. Every host you register shows up here.
            </p>
          </div>

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          {!loaded ? (
            <p className="text-sm text-muted-foreground">Loading…</p>
          ) : hosts.length === 0 ? (
            <div className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border py-16 text-center">
              <Server className="h-6 w-6 text-muted-foreground" />
              <div>
                <p className="text-sm font-medium">Nowhere to run yet</p>
                <p className="text-sm text-muted-foreground">
                  Register a host first — environments hang off one.
                </p>
              </div>
            </div>
          ) : (
            <ul className="flex flex-col gap-3">
              {hosts.map((host) => (
                <HostEnvironments
                  key={host.id}
                  host={host}
                  onAdd={() => openAdd(host)}
                  onCreate={() => openCreate(host)}
                  onEdit={(container) => openEdit(host, container)}
                  onUnregister={(container, destroy) =>
                    setUnregisterTarget({ host, container, destroy })
                  }
                  onRelease={() => setReleaseTarget(host)}
                  releaseNonce={releaseNonce}
                />
              ))}
            </ul>
          )}
        </section>

        <ImagesSection images={images} />
      </div>

      <ContainerSpawnDialog
        key={`spawn-${formKey}`}
        open={spawnOpen}
        onOpenChange={setSpawnOpen}
        host={dialogHost}
        images={
          dialogHost?.service
            ? images.images.filter((image) => image.service)
            : images.images
        }
        onSpawn={spawnContainer}
      />

      <ContainerFormDialog
        key={`container-${formKey}`}
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        host={dialogHost}
        editing={editing}
        onCreate={addContainer}
        onUpdate={editContainer}
      />

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
              Delete your data on {releaseTarget?.name}?
            </AlertDialogTitle>
            <AlertDialogDescription>
              Your directory on that machine is removed — work and scratch
              both, right now. Nothing is archived and nothing can bring it
              back, so take anything you want off it first. Delete your
              containers there before doing this; faber refuses while any of
              them is still registered. Nothing else about your account
              changes, and you can start again whenever you like — though
              faber picks the machine afresh, so it may not be this one.
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
              {releasing ? "Deleting…" : "Delete my data"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={!!unregisterTarget}
        onOpenChange={(open) => !open && setUnregisterTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              {unregisterTarget?.destroy ? "Delete" : "Unregister"}{" "}
              {unregisterTarget?.container.name ?? unregisterTarget?.container.container_ref}?
            </AlertDialogTitle>
            <AlertDialogDescription>
              {unregisterTarget?.destroy
                ? "The container is removed from the machine and everything inside it goes with it. Your work and scratch directories are mounts and survive — anything written outside them does not."
                : "Faber forgets the container and stops offering it as an environment. The container itself keeps running — nothing on the host is touched."}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={unregistering}>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={(event) => {
                event.preventDefault()
                void handleUnregister()
              }}
              disabled={unregistering}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              {unregistering
                ? "Working…"
                : unregisterTarget?.destroy
                  ? "Delete"
                  : "Unregister"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

/**
 * One host's contribution to the list.
 *
 * A direct host *is* the environment, so it renders as a single row with
 * nothing to manage — its registration lives on `/hosts`. A docker host is a
 * container of environments rather than one itself, so it renders as a header
 * over its registrations.
 */
function HostEnvironments({
  host,
  onAdd,
  onCreate,
  onEdit,
  onUnregister,
  onRelease,
  releaseNonce,
}: {
  host: Host
  onAdd: () => void
  onCreate: () => void
  onEdit: (container: HostContainer) => void
  onUnregister: (container: HostContainer, destroy: boolean) => void
  onRelease: () => void
  releaseNonce: number
}) {
  const disabled = !!host.disabled_at
  const tools = toolList(host.last_probe)
  const usage = useHostUsage(host.id, host.service, releaseNonce)

  return (
    <li className="rounded-xl border border-border bg-card">
      <div className="flex items-start justify-between gap-4 px-4 py-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="truncate text-sm font-medium">{host.name}</span>
            {host.service ? <Badge>faber&apos;s</Badge> : null}
            <Badge>{host.exec_mode}</Badge>
            {disabled ? <Badge>disabled</Badge> : null}
          </div>
          {/* Past tense with its age, because that is all a probe is.
              "Never probed" is its own answer — not a default to "down". */}
          <p className="truncate text-xs text-muted-foreground">
            {host.service ? (
              <>
                <Users className="mr-1 inline h-3 w-3" />
                Shared with other people · {observation(host.last_probe)}
              </>
            ) : (
              <>
                {addressLabel(host)} · {observation(host.last_probe)}
              </>
            )}
          </p>
        </div>
        {/* Two ways to get a container, and the difference is who created it:
            Create starts one from an image, Add adopts one already running. */}
        {host.exec_mode === "docker" && host.service ? (
          /* No "Add": registering a container faber did not create means
             pointing it at a ref on a machine it operates, which is either
             somebody else's container or one faber already knows about. */
          <Button size="sm" variant="ghost" className="shrink-0" onClick={onCreate}>
            <Sparkles className="h-4 w-4" />
            Create
          </Button>
        ) : host.exec_mode === "docker" ? (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button size="sm" variant="ghost" className="shrink-0">
                <Plus className="h-4 w-4" />
                Add
                <ChevronDown className="h-3.5 w-3.5 opacity-60" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-64">
              <DropdownMenuItem className="items-start gap-2.5 py-2" onSelect={onCreate}>
                <Sparkles className="mt-0.5 h-4 w-4" />
                <div>
                  <p>Create</p>
                  <p className="text-xs text-muted-foreground">
                    Start a new one from an image.
                  </p>
                </div>
              </DropdownMenuItem>
              <DropdownMenuItem className="items-start gap-2.5 py-2" onSelect={onAdd}>
                <Container className="mt-0.5 h-4 w-4" />
                <div>
                  <p>Add</p>
                  <p className="text-xs text-muted-foreground">
                    Point faber at one you already run.
                  </p>
                </div>
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        ) : null}
      </div>

      {host.service && host.quota ? (
        <div className="border-t border-border px-4 py-3">
          <div className="flex flex-wrap items-baseline justify-between gap-2">
            <p className="text-xs font-medium">Your share of this machine</p>
            {host.quota.expires_at ? (
              <Badge>
                {host.quota.granted ? "grant" : "limits"} expire{" "}
                {new Date(host.quota.expires_at).toLocaleDateString()}
              </Badge>
            ) : null}
          </div>
          <p className="mt-1 text-xs text-muted-foreground">
            {cores(host.quota.cpu_millis)} · {bytes(host.quota.memory_bytes)} RAM ·{" "}
            {host.containers.length}/{count(host.quota.container_max)} containers
          </p>

          {/* Storage is the only limit that can stop work mid-flight, and the
              only one whose current level is worth showing next to the grant:
              CPU and memory degrade or kill, storage refuses the next write. */}
          <div className="mt-2 flex flex-col gap-1">
            <StorageMeter
              used={usage?.storage_bytes ?? null}
              limit={host.quota.storage_bytes}
            />
            <p className="text-xs text-muted-foreground">
              {usage?.storage_bytes == null
                ? `${bytes(host.quota.storage_bytes)} of disk granted`
                : `${bytes(usage.storage_bytes)} of ${bytes(host.quota.storage_bytes)} disk used`}
            </p>
          </div>

          {usage?.scratch_path ? (
            <p className="mt-2 truncate font-mono text-[11px] text-muted-foreground">
              scratch: {usage.scratch_path}
            </p>
          ) : null}

          {/* Only once there is something to give back. Before the first
              launch there is no directory on the machine, and offering to
              delete one would be offering to delete nothing. */}
          {usage?.materialised ? (
            <div className="mt-3 border-t border-border pt-3">
              <Button
                size="sm"
                variant="ghost"
                className="h-auto px-0 text-xs text-muted-foreground hover:text-destructive"
                onClick={onRelease}
              >
                <Eraser className="h-3.5 w-3.5" />
                Give back my space on this machine
              </Button>
            </div>
          ) : null}
        </div>
      ) : null}

      {tools.length > 0 ? (
        <div className="flex flex-wrap items-center gap-1.5 px-4 pb-3">
          {tools.map(([name, version]) => (
            <span
              key={name}
              className="rounded-md bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground"
            >
              {name} {version}
            </span>
          ))}
        </div>
      ) : null}

      {host.exec_mode === "docker" ? (
        <div className="border-t border-border px-4 py-3">
          {host.containers.length === 0 ? (
            <p className="text-xs text-muted-foreground">
              {host.service
                ? "Nothing of yours here yet. Create one and faber makes you a directory on the machine at the same time."
                : "No containers registered on this host."}
            </p>
          ) : (
            <ul className="flex flex-col gap-1">
              {host.containers.map((container) => (
                <li
                  key={container.id}
                  className="flex items-center justify-between gap-3 rounded-lg px-2 py-1.5 hover:bg-muted/50"
                >
                  <div className="flex min-w-0 items-center gap-2">
                    <Container className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                    <span className="truncate text-[13px]">
                      {container.name ?? container.container_ref}
                    </span>
                    <span className="truncate font-mono text-[11px] text-muted-foreground">
                      {container.root_path}
                    </span>
                  </div>
                  <div className="flex shrink-0 items-center gap-1">
                    {/* Not editable on a shared machine. `root_path` there is
                        where faber mounted the tenant's own directory when it
                        created the container, so repointing the row afterwards
                        aims the recorded root at somewhere nothing is
                        mounted. */}
                    {host.service ? null : (
                      <Button
                        size="icon-sm"
                        variant="ghost"
                        aria-label={`Edit ${container.container_ref}`}
                        onClick={() => onEdit(container)}
                      >
                        <Pencil className="h-3.5 w-3.5" />
                      </Button>
                    )}
                    {/* Offered exactly when the server will accept it: faber
                        destroys only what it created. On a shared machine that
                        is also the only offer, because forgetting a
                        registration there leaves a container running that
                        nothing counts and nobody can see — the count bills on
                        the row. */}
                    {container.managed ? (
                      <Button
                        size="icon-sm"
                        variant="ghost"
                        aria-label={`Delete ${container.container_ref}`}
                        title="Delete — the container is removed from the machine"
                        onClick={() => onUnregister(container, true)}
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                      </Button>
                    ) : null}
                    {host.service ? null : (
                      <Button
                        size="icon-sm"
                        variant="ghost"
                        aria-label={`Unregister ${container.container_ref}`}
                        title="Unregister — the container itself keeps running"
                        onClick={() => onUnregister(container, false)}
                      >
                        <Unplug className="h-3.5 w-3.5" />
                      </Button>
                    )}
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : null}
    </li>
  )
}

/**
 * Storage used against storage granted.
 *
 * Nothing is drawn when either half is missing. An unlimited grant has no bar
 * to fill, and an unread counter is the machine declining to answer rather
 * than a zero — a bar from a guess would be worse than no bar on the one
 * number a user checks before deciding they are out of space.
 */
function StorageMeter({ used, limit }: { used: number | null; limit: number | null }) {
  if (used === null || limit === null || limit <= 0) return null
  const fraction = Math.min(1, used / limit)
  return (
    <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
      <div
        className="h-full rounded-full bg-foreground/40"
        style={{ width: `${Math.round(fraction * 100)}%` }}
      />
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

/**
 * Spawn templates: a saved reference plus its defaults, and the thing Create
 * starts a container from.
 *
 * It sits with the environments rather than the hosts because an image
 * describes a container, and a container is an environment.
 */
function ImagesSection({ images: source }: { images: ReturnType<typeof useImages> }) {
  const { images, loaded, error, addImage, editImage, removeImage } = source

  const [dialogOpen, setDialogOpen] = React.useState(false)
  const [editing, setEditing] = React.useState<Image | null>(null)
  const [formKey, setFormKey] = React.useState(0)
  const [deleteTarget, setDeleteTarget] = React.useState<Image | null>(null)
  const [deleting, setDeleting] = React.useState(false)

  const handleDelete = async () => {
    if (!deleteTarget) return
    setDeleting(true)
    try {
      await removeImage(deleteTarget.id)
      setDeleteTarget(null)
    } catch {
      // The hook holds the message; the dialog stays open for a retry.
    } finally {
      setDeleting(false)
    }
  }

  const openCreate = () => {
    setEditing(null)
    setFormKey((k) => k + 1)
    setDialogOpen(true)
  }

  const openEdit = (image: Image) => {
    setEditing(image)
    setFormKey((k) => k + 1)
    setDialogOpen(true)
  }

  return (
    <section className="flex flex-col gap-6 border-t border-border pt-10">
      <div className="flex items-center justify-between gap-4">
        <div>
          <h2 className="text-lg font-semibold tracking-tight">Images</h2>
          <p className="text-sm text-muted-foreground">
            Saved templates for containers you start yourself. The ones faber
            provides are here too — they are the only templates a shared host
            will start.
          </p>
        </div>
        <Button size="sm" onClick={openCreate}>
          <Plus className="h-4 w-4" />
          Add image
        </Button>
      </div>

      {error ? <p className="text-sm text-destructive">{error}</p> : null}

      {!loaded ? (
        <p className="text-sm text-muted-foreground">Loading…</p>
      ) : images.length === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border py-12 text-center">
          <Layers className="h-6 w-6 text-muted-foreground" />
          <p className="text-sm text-muted-foreground">No images saved.</p>
        </div>
      ) : (
        <ul className="flex flex-col gap-2">
          {images.map((image) => (
            <li
              key={image.id}
              className="flex items-center justify-between gap-4 rounded-xl border border-border bg-card px-4 py-3"
            >
              <div className="flex min-w-0 items-center gap-2.5">
                <Box className="h-4 w-4 shrink-0 text-muted-foreground" />
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <p className="truncate text-sm font-medium">{image.name}</p>
                    {image.service ? <Badge>faber&apos;s</Badge> : null}
                  </div>
                  <p className="truncate text-xs text-muted-foreground">
                    {image.reference} · {image.default_root_path}
                  </p>
                </div>
              </div>
              {/* A template faber provides is offered, not owned: editing or
                  deleting it would change what every other user can start. */}
              {image.service ? null : (
                <div className="flex shrink-0 items-center gap-1">
                  <Button
                    size="icon-sm"
                    variant="ghost"
                    aria-label={`Edit ${image.name}`}
                    onClick={() => openEdit(image)}
                  >
                    <Pencil className="h-4 w-4" />
                  </Button>
                  <Button
                    size="icon-sm"
                    variant="ghost"
                    aria-label={`Delete ${image.name}`}
                    onClick={() => setDeleteTarget(image)}
                  >
                    <Trash2 className="h-4 w-4" />
                  </Button>
                </div>
              )}
            </li>
          ))}
        </ul>
      )}

      <ImageFormDialog
        key={`image-${formKey}`}
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        editing={editing}
        onCreate={addImage}
        onUpdate={editImage}
      />

      <AlertDialog open={!!deleteTarget} onOpenChange={(open) => !open && setDeleteTarget(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete {deleteTarget?.name}?</AlertDialogTitle>
            <AlertDialogDescription>
              Containers you already started from this template are unaffected —
              nothing links them back to it.
            </AlertDialogDescription>
          </AlertDialogHeader>
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
    </section>
  )
}
