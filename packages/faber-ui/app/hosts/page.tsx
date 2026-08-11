"use client"

import * as React from "react"
import {
  Box,
  Container,
  Layers,
  Pencil,
  Plus,
  Power,
  Server,
  Trash2,
  Unplug,
} from "lucide-react"

import {
  faber,
  FaberError,
  type CreateImageRequest,
  type Host,
  type HostContainer,
  type Image,
  type UpdateImageRequest,
  type Uuid,
} from "@/lib/api"
import { useHosts } from "@/lib/hosts/use-hosts"
import { addressLabel } from "@/lib/hosts/posture"
import { Button } from "@/components/ui/button"
import {
  ContainerFormDialog,
  HostFormDialog,
  ImageFormDialog,
} from "@/components/hosts/host-dialogs"
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
 * Registration surface: what exists, and how faber addresses it.
 *
 * The read-side view of the same rows — what each mode buys, and what the last
 * probe observed — lives on `/environments`. Keeping them apart is what stops
 * this page's registration detail from crowding out the trust posture that
 * page exists to state.
 */
export default function HostsPage() {
  const {
    hosts,
    loaded,
    error,
    addHost,
    editHost,
    removeHost,
    addContainer,
    editContainer,
    unregisterContainer,
  } = useHosts()

  const [hostDialogOpen, setHostDialogOpen] = React.useState(false)
  const [editingHost, setEditingHost] = React.useState<Host | null>(null)
  const [deleteTarget, setDeleteTarget] = React.useState<Host | null>(null)
  const [deleting, setDeleting] = React.useState(false)

  const [containerDialogOpen, setContainerDialogOpen] = React.useState(false)
  const [containerHost, setContainerHost] = React.useState<Host | null>(null)
  const [editingContainer, setEditingContainer] = React.useState<HostContainer | null>(null)

  // Bumped on every open so the dialogs remount with a fresh draft — the same
  // trick the models page uses.
  const [formKey, setFormKey] = React.useState(0)
  const bump = () => setFormKey((k) => k + 1)

  const openCreateHost = () => {
    setEditingHost(null)
    bump()
    setHostDialogOpen(true)
  }

  const openEditHost = (host: Host) => {
    setEditingHost(host)
    bump()
    setHostDialogOpen(true)
  }

  const openRegisterContainer = (host: Host) => {
    setContainerHost(host)
    setEditingContainer(null)
    bump()
    setContainerDialogOpen(true)
  }

  const openEditContainer = (host: Host, container: HostContainer) => {
    setContainerHost(host)
    setEditingContainer(container)
    bump()
    setContainerDialogOpen(true)
  }

  const handleDelete = async () => {
    if (!deleteTarget) return
    setDeleting(true)
    try {
      await removeHost(deleteTarget.id)
      setDeleteTarget(null)
    } catch {
      // The dialog stays open with the target set so the user can retry.
    } finally {
      setDeleting(false)
    }
  }

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto flex w-full max-w-2xl flex-col gap-10 px-4 py-10">
        <section className="flex flex-col gap-6">
          <div className="flex items-center justify-between gap-4">
            <div>
              <h1 className="text-lg font-semibold tracking-tight">Hosts</h1>
              <p className="text-sm text-muted-foreground">
                The machines faber can reach. Every execution mode bottoms out in
                one of these.
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
                <p className="text-sm font-medium">No hosts yet</p>
                <p className="text-sm text-muted-foreground">
                  Register one to give the agent somewhere to run.
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
                  onEdit={() => openEditHost(host)}
                  onDelete={() => setDeleteTarget(host)}
                  onToggleDisabled={() =>
                    editHost(host.id, { disabled: !host.disabled_at }).catch(() => {
                      // Left as-is on failure; the row still shows the server's
                      // last known answer.
                    })
                  }
                  onRegisterContainer={() => openRegisterContainer(host)}
                  onEditContainer={(container) => openEditContainer(host, container)}
                  onUnregisterContainer={(container) =>
                    unregisterContainer(host.id, container.id).catch(() => {})
                  }
                />
              ))}
            </ul>
          )}
        </section>

        <ImagesSection />
      </div>

      <HostFormDialog
        key={`host-${formKey}`}
        open={hostDialogOpen}
        onOpenChange={setHostDialogOpen}
        editing={editingHost}
        onCreate={addHost}
        onUpdate={editHost}
      />

      <ContainerFormDialog
        key={`container-${formKey}`}
        open={containerDialogOpen}
        onOpenChange={setContainerDialogOpen}
        host={containerHost}
        editing={editingContainer}
        onCreate={addContainer}
        onUpdate={editContainer}
      />

      <AlertDialog open={!!deleteTarget} onOpenChange={(open) => !open && setDeleteTarget(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete {deleteTarget?.name}?</AlertDialogTitle>
            <AlertDialogDescription>
              This drops the registration, its container registrations, and its
              probe history. Nothing on the machine itself is touched. Disabling
              the host instead keeps all of it and just takes it out of use.
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

function HostCard({
  host,
  onEdit,
  onDelete,
  onToggleDisabled,
  onRegisterContainer,
  onEditContainer,
  onUnregisterContainer,
}: {
  host: Host
  onEdit: () => void
  onDelete: () => void
  onToggleDisabled: () => void
  onRegisterContainer: () => void
  onEditContainer: (container: HostContainer) => void
  onUnregisterContainer: (container: HostContainer) => void
}) {
  const disabled = !!host.disabled_at

  return (
    <li className="rounded-xl border border-border bg-card">
      <div className="flex items-start justify-between gap-4 px-4 py-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="truncate text-sm font-medium">{host.name}</span>
            <Badge>{host.transport}</Badge>
            <Badge>{host.exec_mode}</Badge>
            {disabled ? <Badge>disabled</Badge> : null}
          </div>
          <p className="truncate text-xs text-muted-foreground">
            {addressLabel(host)}
            {host.exec_mode === "docker"
              ? ` · ${host.docker_endpoint ?? "local socket"}`
              : ""}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          <Button
            size="icon-sm"
            variant="ghost"
            aria-label={disabled ? `Enable ${host.name}` : `Disable ${host.name}`}
            title={disabled ? "Enable" : "Disable"}
            onClick={onToggleDisabled}
          >
            <Power className="h-4 w-4" />
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

      {host.exec_mode === "docker" ? (
        <div className="border-t border-border px-4 py-3">
          <div className="mb-2 flex items-center justify-between gap-4">
            <span className="text-[10.5px] font-medium uppercase tracking-[0.12em] text-muted-foreground/70">
              Containers
            </span>
            <Button size="sm" variant="ghost" onClick={onRegisterContainer}>
              <Plus className="h-4 w-4" />
              Register
            </Button>
          </div>

          {host.containers.length === 0 ? (
            <p className="text-xs text-muted-foreground">
              None registered. Faber never creates containers — register one you
              already run.
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
                    <span className="truncate text-xs text-muted-foreground">
                      {container.root_path}
                    </span>
                  </div>
                  <div className="flex shrink-0 items-center gap-1">
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      aria-label={`Edit ${container.container_ref}`}
                      onClick={() => onEditContainer(container)}
                    >
                      <Pencil className="h-3.5 w-3.5" />
                    </Button>
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      aria-label={`Unregister ${container.container_ref}`}
                      title="Unregister — the container itself keeps running"
                      onClick={() => onUnregisterContainer(container)}
                    >
                      <Unplug className="h-3.5 w-3.5" />
                    </Button>
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
 * Spawn templates. Registration only: faber has no spawn route yet, so an image
 * is a saved reference plus its defaults, not something you can start from
 * here.
 */
function ImagesSection() {
  const [images, setImages] = React.useState<Image[]>([])
  const [loaded, setLoaded] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  const [dialogOpen, setDialogOpen] = React.useState(false)
  const [editing, setEditing] = React.useState<Image | null>(null)
  const [formKey, setFormKey] = React.useState(0)
  const [deleteTarget, setDeleteTarget] = React.useState<Image | null>(null)
  const [deleting, setDeleting] = React.useState(false)

  React.useEffect(() => {
    let cancelled = false
    void faber
      .listImages()
      .then((rows) => {
        if (!cancelled) setImages(rows)
      })
      .catch(() => {
        if (!cancelled) setError("Could not load images.")
      })
      .finally(() => {
        if (!cancelled) setLoaded(true)
      })
    return () => {
      cancelled = true
    }
  }, [])

  const addImage = React.useCallback(async (body: CreateImageRequest): Promise<Image> => {
    const created = await faber.createImage(body)
    setImages((prev) => [...prev, created])
    return created
  }, [])

  const editImage = React.useCallback(
    async (id: Uuid, patch: UpdateImageRequest): Promise<Image> => {
      const updated = await faber.updateImage(id, patch)
      setImages((prev) => prev.map((image) => (image.id === id ? updated : image)))
      return updated
    },
    [],
  )

  const handleDelete = async () => {
    if (!deleteTarget) return
    setDeleting(true)
    try {
      await faber.deleteImage(deleteTarget.id)
      setImages((prev) => prev.filter((image) => image.id !== deleteTarget.id))
      setDeleteTarget(null)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to delete the image")
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
            Saved templates for containers you start yourself. Faber records the
            reference and its defaults — it does not own anything you spawn.
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
                  <p className="truncate text-sm font-medium">{image.name}</p>
                  <p className="truncate text-xs text-muted-foreground">
                    {image.reference} · {image.default_root_path}
                  </p>
                </div>
              </div>
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
