"use client"

import * as React from "react"

import {
  FaberError,
  type CreateServiceHostRequest,
  type CreateServiceImageRequest,
  type GrantRequest,
  type Limits,
  type ServiceHost,
  type ServiceImage,
  type Tenant,
  type UpdateServiceHostRequest,
  type UpdateServiceImageRequest,
  type Uuid,
} from "@/lib/api"
import {
  bytesToGib,
  coresToMillis,
  gibToBytes,
  millisToCores,
  toInteger,
} from "@/lib/hosts/limits"
import { AnimatedField } from "@/components/ui/animated-field"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/responsive-dialog"

/**
 * The four ceilings, as four fields.
 *
 * Every one of them is labelled for what an empty box means, because empty
 * means *unlimited* here rather than "unchanged" — a grant replaces a host's
 * defaults as a whole row, so clearing a field is a decision and not an
 * omission.
 */
type LimitsForm = {
  cores: string
  memory: string
  storage: string
  containers: string
}

const EMPTY_LIMITS: LimitsForm = {
  cores: "",
  memory: "",
  storage: "",
  containers: "",
}

function limitsForm(limits: Limits): LimitsForm {
  return {
    cores: millisToCores(limits.cpu_millis),
    memory: bytesToGib(limits.memory_bytes),
    storage: bytesToGib(limits.storage_bytes),
    containers: limits.container_max === null ? "" : String(limits.container_max),
  }
}

function limitsFrom(form: LimitsForm): Limits {
  return {
    cpu_millis: coresToMillis(form.cores),
    memory_bytes: gibToBytes(form.memory),
    storage_bytes: gibToBytes(form.storage),
    container_max: toInteger(form.containers),
  }
}

function LimitFields({
  prefix,
  form,
  onChange,
}: {
  prefix: string
  form: LimitsForm
  onChange: (next: LimitsForm) => void
}) {
  const set = (patch: Partial<LimitsForm>) => onChange({ ...form, ...patch })

  return (
    <div className="flex flex-col gap-4">
      <AnimatedField
        id={`${prefix}-cores`}
        label="CPU"
        value={form.cores}
        onChange={(v) => set({ cores: v })}
        placeholder="unlimited"
        hint="Cores, as a hard ceiling. An idle machine lends nobody more."
      />
      <AnimatedField
        id={`${prefix}-memory`}
        label="Memory"
        value={form.memory}
        onChange={(v) => set({ memory: v })}
        placeholder="unlimited"
        hint="GiB. Exceeding it is a kill inside this user's own slice, never anyone else's."
      />
      <AnimatedField
        id={`${prefix}-storage`}
        label="Storage"
        value={form.storage}
        onChange={(v) => set({ storage: v })}
        placeholder="unlimited"
        hint="GiB, reserved rather than overcommitted — a raise the filesystem cannot hold is refused."
      />
      <AnimatedField
        id={`${prefix}-containers`}
        label="Containers"
        value={form.containers}
        onChange={(v) => set({ containers: v })}
        placeholder="unlimited"
        hint="Registered containers, running or not — a stopped one still holds its directory."
      />
    </div>
  )
}

// ---------------------------------------------------------------------------
// Host
// ---------------------------------------------------------------------------

type HostForm = {
  name: string
  docker_endpoint: string
  user_data_root: string
  defaults: LimitsForm
}

const EMPTY_HOST: HostForm = {
  name: "",
  docker_endpoint: "unix:///var/run/docker.sock",
  user_data_root: "",
  defaults: EMPTY_LIMITS,
}

/**
 * Registering or editing a machine faber operates.
 *
 * The form asks for nothing about transport or exec mode: a service host is
 * faber's own machine talking to its own docker socket, which is what makes
 * writing a cgroup slice and a filesystem quota possible in the first place.
 *
 * Nothing here prepares the machine. The controllers, the tenant slice, and a
 * data root on a filesystem with project quotas all have to exist beforehand —
 * their absence shows up later as a launch that fails confusingly, so the
 * hints say so at the point where somebody is typing the path.
 */
export function ServiceHostFormDialog({
  open,
  onOpenChange,
  editing,
  onCreate,
  onUpdate,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  editing: ServiceHost | null
  onCreate: (body: CreateServiceHostRequest) => Promise<ServiceHost>
  onUpdate: (id: Uuid, patch: UpdateServiceHostRequest) => Promise<ServiceHost>
}) {
  const [form, setForm] = React.useState<HostForm>(() =>
    editing
      ? {
          name: editing.name,
          docker_endpoint: editing.docker_endpoint ?? "",
          user_data_root: editing.user_data_root ?? "",
          defaults: limitsForm(editing.defaults),
        }
      : EMPTY_HOST,
  )
  const [submitting, setSubmitting] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  // Moving the root would leave every tenant's directory, quota, and work in
  // progress where it is. The server refuses it; the field says so first.
  const rootLocked = !!editing && editing.tenants > 0

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    setSubmitting(true)
    setError(null)
    try {
      const body = {
        name: form.name.trim(),
        docker_endpoint: form.docker_endpoint.trim(),
        user_data_root: form.user_data_root.trim(),
        defaults: limitsFrom(form.defaults),
      }
      if (editing) {
        const { user_data_root, ...rest } = body
        await onUpdate(editing.id, rootLocked ? rest : { ...rest, user_data_root })
      } else {
        await onCreate(body)
      }
      onOpenChange(false)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to save the host")
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{editing ? `Edit ${editing.name}` : "Add a service host"}</DialogTitle>
        </DialogHeader>

        <form onSubmit={handleSubmit} className="flex flex-col gap-4">
          <AnimatedField
            id="service-host-name"
            label="Name"
            value={form.name}
            onChange={(v) => setForm((f) => ({ ...f, name: v }))}
            placeholder="shared-1"
            required
          />

          <AnimatedField
            id="service-host-endpoint"
            label="Docker endpoint"
            value={form.docker_endpoint}
            onChange={(v) => setForm((f) => ({ ...f, docker_endpoint: v }))}
            placeholder="unix:///var/run/docker.sock"
            hint="Named explicitly — faber never falls back to the process's own docker context."
            required
          />

          <AnimatedField
            id="service-host-data-root"
            label="Tenant data root"
            value={form.user_data_root}
            onChange={(v) => setForm((f) => ({ ...f, user_data_root: v }))}
            placeholder="/srv/faber"
            disabled={rootLocked}
            hint={
              rootLocked
                ? "Fixed: tenants already have directories under this root, and faber does not move them."
                : "One directory per tenant lives here, each carrying the project quota that is their storage limit."
            }
            required={!editing}
          />

          <div className="rounded-lg border border-border p-3">
            <p className="mb-3 text-sm font-medium">Default limits</p>
            <p className="mb-3 text-xs text-muted-foreground">
              What every tenant gets unless they have a grant of their own. An
              empty field is unlimited.
            </p>
            <LimitFields
              prefix="service-host"
              form={form.defaults}
              onChange={(defaults) => setForm((f) => ({ ...f, defaults }))}
            />
          </div>

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" loading={submitting}>
              {editing ? "Save" : "Add host"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

// ---------------------------------------------------------------------------
// Grant
// ---------------------------------------------------------------------------

/**
 * One user's own ceiling on one host.
 *
 * The dialog opens pre-filled with what they resolve to today — the host's
 * defaults when they have no grant — because a grant is a whole row and the
 * pre-fill is what keeps "raise their disk" from silently granting unlimited
 * everything else.
 */
export function GrantDialog({
  open,
  onOpenChange,
  host,
  tenant,
  onGrant,
  onRevoke,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  host: ServiceHost | null
  tenant: Tenant | null
  onGrant: (hostId: Uuid, userId: Uuid, body: GrantRequest) => Promise<void>
  onRevoke: (hostId: Uuid, userId: Uuid) => Promise<void>
}) {
  const [form, setForm] = React.useState<LimitsForm>(() =>
    tenant ? limitsForm(tenant.quota) : EMPTY_LIMITS,
  )
  const [expires, setExpires] = React.useState(
    tenant?.quota.expires_at ? tenant.quota.expires_at.slice(0, 10) : "",
  )
  const [note, setNote] = React.useState(tenant?.quota.note ?? "")
  const [submitting, setSubmitting] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  if (!host || !tenant) return null

  const submit = async (event: React.FormEvent) => {
    event.preventDefault()
    setSubmitting(true)
    setError(null)
    try {
      await onGrant(host.id, tenant.user_id, {
        ...limitsFrom(form),
        // A date, widened to the end of that day in the browser's own zone —
        // an operator typing a day means the whole of it.
        expires_at: expires ? new Date(`${expires}T23:59:59`).toISOString() : null,
        note: note.trim() || null,
      })
      onOpenChange(false)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to save the grant")
    } finally {
      setSubmitting(false)
    }
  }

  const revoke = async () => {
    setSubmitting(true)
    setError(null)
    try {
      await onRevoke(host.id, tenant.user_id)
      onOpenChange(false)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to revoke the grant")
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Limits for subject {tenant.subject_id ?? "—"}</DialogTitle>
        </DialogHeader>

        <form onSubmit={submit} className="flex flex-col gap-4">
          <p className="text-xs text-muted-foreground">
            A grant replaces {host.name}&apos;s defaults for this user as a
            whole. Every field below is the answer in full, so an empty one
            grants unlimited rather than falling back to the default.
          </p>

          <LimitFields prefix="grant" form={form} onChange={setForm} />

          <AnimatedField
            id="grant-expires"
            label="Expires"
            type="date"
            value={expires}
            onChange={setExpires}
            hint="Left empty, the grant stands until it is revoked. An expiry takes effect the moment it passes, which can shrink a limit under work already running."
          />

          <AnimatedField
            id="grant-note"
            label="Note"
            value={note}
            onChange={setNote}
            placeholder="why"
            hint="Kept with the grant, which outlives the conversation that produced it."
          />

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          <DialogFooter className="sm:justify-between">
            {tenant.quota.granted ? (
              <Button
                type="button"
                variant="ghost"
                onClick={() => void revoke()}
                disabled={submitting}
              >
                Back to defaults
              </Button>
            ) : (
              <span />
            )}
            <div className="flex gap-2">
              <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
                Cancel
              </Button>
              <Button type="submit" loading={submitting}>
                Grant
              </Button>
            </div>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

// ---------------------------------------------------------------------------
// Service image
// ---------------------------------------------------------------------------

type ImageForm = {
  name: string
  reference: string
  default_root_path: string
}

const EMPTY_IMAGE: ImageForm = {
  name: "",
  reference: "",
  default_root_path: "/work",
}

/**
 * A template faber provides.
 *
 * These are the only images a service host will start. A user naming an
 * arbitrary registry reference on faber's machine would be arbitrary code,
 * unbounded pull bandwidth, and image layers sitting outside every tenant's
 * quota — so this list is also the list of what anyone can run there.
 */
export function ServiceImageFormDialog({
  open,
  onOpenChange,
  editing,
  onCreate,
  onUpdate,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  editing: ServiceImage | null
  onCreate: (body: CreateServiceImageRequest) => Promise<ServiceImage>
  onUpdate: (id: Uuid, patch: UpdateServiceImageRequest) => Promise<ServiceImage>
}) {
  const [form, setForm] = React.useState<ImageForm>(() =>
    editing
      ? {
          name: editing.name,
          reference: editing.reference,
          default_root_path: editing.default_root_path,
        }
      : EMPTY_IMAGE,
  )
  const [submitting, setSubmitting] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    setSubmitting(true)
    setError(null)
    try {
      const body = {
        name: form.name.trim(),
        reference: form.reference.trim(),
        default_root_path: form.default_root_path.trim(),
      }
      if (editing) await onUpdate(editing.id, body)
      else await onCreate(body)
      onOpenChange(false)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to save the image")
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>
            {editing ? `Edit ${editing.name}` : "Add a service image"}
          </DialogTitle>
        </DialogHeader>

        <form onSubmit={handleSubmit} className="flex flex-col gap-4">
          <AnimatedField
            id="service-image-name"
            label="Name"
            value={form.name}
            onChange={(v) => setForm((f) => ({ ...f, name: v }))}
            placeholder="faber-base"
            required
          />
          <AnimatedField
            id="service-image-reference"
            label="Reference"
            value={form.reference}
            onChange={(v) => setForm((f) => ({ ...f, reference: v }))}
            placeholder="ghcr.io/acme/dev:latest"
            hint="Pulled with faber's own bandwidth onto faber's own machine, which is why only these are allowed there."
            required
          />
          <AnimatedField
            id="service-image-root"
            label="Default root path"
            value={form.default_root_path}
            onChange={(v) => setForm((f) => ({ ...f, default_root_path: v }))}
            placeholder="/work"
            hint="Absolute. Where the tenant's own directory is mounted inside the container."
            required
          />

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" loading={submitting}>
              {editing ? "Save" : "Add image"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
