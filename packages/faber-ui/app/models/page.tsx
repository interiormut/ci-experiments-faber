"use client"

import * as React from "react"
import { Cpu, Pencil, Plus, Trash2 } from "lucide-react"

import {
  faber,
  FaberError,
  type CreateModelRequest,
  type Credential,
  type ModelConfig,
  type ReasoningHistory,
  type UpdateModelRequest,
  type Uuid,
  type Wire,
} from "@/lib/api"
import { useAppShell } from "@/components/shell/app-shell"
import { Button } from "@/components/ui/button"
import { AnimatedField } from "@/components/ui/animated-field"
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
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/responsive-dialog"

const WIRE_OPTIONS: Wire[] = ["anthropic", "openai"]

/** `""` is "say nothing", which leaves the wire's own default. */
const REASONING_OPTIONS: { value: ReasoningHistory | ""; label: string }[] = [
  { value: "", label: "Provider default" },
  { value: "full", label: "Send reasoning and signature" },
  { value: "text", label: "Send reasoning without signature" },
  { value: "omitted", label: "Don't send reasoning" },
]

const REASONING_KEY = "reasoning_history"

function reasoningOf(capabilities: unknown): ReasoningHistory | "" {
  if (typeof capabilities !== "object" || capabilities === null) return ""
  const value = (capabilities as Record<string, unknown>)[REASONING_KEY]
  return value === "full" || value === "text" || value === "omitted" ? value : ""
}

/**
 * Sets the key without disturbing the rest of the blob — `capabilities` is a
 * free-form column this form owns only one field of.
 */
function withReasoning(
  capabilities: unknown,
  reasoning: ReasoningHistory | "",
): Record<string, unknown> {
  const base =
    typeof capabilities === "object" && capabilities !== null && !Array.isArray(capabilities)
      ? { ...(capabilities as Record<string, unknown>) }
      : {}
  if (reasoning) base[REASONING_KEY] = reasoning
  else delete base[REASONING_KEY]
  return base
}

type FormState = {
  alias: string
  wire: Wire
  wire_id: string
  base_url: string
  family: string
  credential_id: Uuid | ""
  reasoning_history: ReasoningHistory | ""
  /** Carried whole so saving one field doesn't drop the others. */
  capabilities: unknown
}

const EMPTY_FORM: FormState = {
  alias: "",
  wire: "anthropic",
  wire_id: "",
  base_url: "",
  family: "",
  credential_id: "",
  reasoning_history: "",
  capabilities: {},
}

function formFromModel(model: ModelConfig): FormState {
  return {
    alias: model.alias,
    wire: model.wire,
    wire_id: model.wire_id,
    base_url: model.base_url,
    family: model.family ?? "",
    credential_id: model.credential_id ?? "",
    reasoning_history: reasoningOf(model.capabilities),
    capabilities: model.capabilities,
  }
}

function requestFromForm(form: FormState): CreateModelRequest {
  return {
    alias: form.alias.trim(),
    wire: form.wire,
    wire_id: form.wire_id.trim(),
    base_url: form.base_url.trim(),
    family: form.family.trim() ? form.family.trim() : null,
    credential_id: form.credential_id || null,
    capabilities: withReasoning(
      form.capabilities,
      form.reasoning_history,
    ) as CreateModelRequest["capabilities"],
  }
}

export default function ModelsPage() {
  const { models, modelsLoaded, addModel, editModel, removeModel } = useAppShell()

  const [credentials, setCredentials] = React.useState<Credential[]>([])

  React.useEffect(() => {
    let cancelled = false
    void faber
      .listCredentials()
      .then((rows) => {
        if (!cancelled) setCredentials(rows)
      })
      .catch(() => {
        // Credential labels are a nicety here — a failed fetch just falls
        // back to showing the raw id, and the dialog's picker degrades to
        // "None" only.
      })
    return () => {
      cancelled = true
    }
  }, [])

  const credentialLabel = React.useCallback(
    (id: Uuid | null) => {
      if (!id) return null
      return credentials.find((c) => c.id === id)?.label ?? id
    },
    [credentials],
  )

  const [dialogOpen, setDialogOpen] = React.useState(false)
  const [editing, setEditing] = React.useState<ModelConfig | null>(null)
  const [deleteTarget, setDeleteTarget] = React.useState<ModelConfig | null>(null)
  const [deleting, setDeleting] = React.useState(false)

  // Bumped on every open so `ModelFormDialog` remounts with a fresh draft —
  // simpler than an effect that resets its state after the fact.
  const [formKey, setFormKey] = React.useState(0)

  const openCreate = () => {
    setEditing(null)
    setFormKey((k) => k + 1)
    setDialogOpen(true)
  }

  const openEdit = (model: ModelConfig) => {
    setEditing(model)
    setFormKey((k) => k + 1)
    setDialogOpen(true)
  }

  const handleDelete = async () => {
    if (!deleteTarget) return
    setDeleting(true)
    try {
      await removeModel(deleteTarget.id)
      setDeleteTarget(null)
    } catch {
      // The dialog stays open with the target set so the user can retry.
    } finally {
      setDeleting(false)
    }
  }

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto flex w-full max-w-2xl flex-col gap-6 px-4 py-10">
        <div className="flex items-center justify-between gap-4">
          <div>
            <h1 className="text-lg font-semibold tracking-tight">Models</h1>
            <p className="text-sm text-muted-foreground">
              The models available to send messages with, across every thread.
            </p>
          </div>
          <Button size="sm" onClick={openCreate}>
            <Plus className="h-4 w-4" />
            Add model
          </Button>
        </div>

        {!modelsLoaded ? (
          <p className="text-sm text-muted-foreground">Loading…</p>
        ) : models.length === 0 ? (
          <div className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border py-16 text-center">
            <Cpu className="h-6 w-6 text-muted-foreground" />
            <div>
              <p className="text-sm font-medium">No models yet</p>
              <p className="text-sm text-muted-foreground">
                Add one to start sending messages.
              </p>
            </div>
            <Button size="sm" onClick={openCreate}>
              <Plus className="h-4 w-4" />
              Add model
            </Button>
          </div>
        ) : (
          <ul className="flex flex-col gap-2">
            {models.map((model) => (
              <li
                key={model.id}
                className="flex items-center justify-between gap-4 rounded-xl border border-border bg-card px-4 py-3"
              >
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="truncate text-sm font-medium">{model.alias}</span>
                    <span className="shrink-0 rounded-full bg-muted px-2 py-0.5 text-[11px] font-medium text-muted-foreground">
                      {model.wire}
                    </span>
                  </div>
                  <p className="truncate text-xs text-muted-foreground">
                    {model.wire_id} · {model.base_url}
                    {credentialLabel(model.credential_id) ? ` · ${credentialLabel(model.credential_id)}` : ""}
                  </p>
                </div>
                <div className="flex shrink-0 items-center gap-1">
                  <Button
                    size="icon-sm"
                    variant="ghost"
                    aria-label={`Edit ${model.alias}`}
                    onClick={() => openEdit(model)}
                  >
                    <Pencil className="h-4 w-4" />
                  </Button>
                  <Button
                    size="icon-sm"
                    variant="ghost"
                    aria-label={`Delete ${model.alias}`}
                    onClick={() => setDeleteTarget(model)}
                  >
                    <Trash2 className="h-4 w-4" />
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>

      <ModelFormDialog
        key={formKey}
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        editing={editing}
        credentials={credentials}
        onCreate={addModel}
        onUpdate={editModel}
      />

      <AlertDialog open={!!deleteTarget} onOpenChange={(open) => !open && setDeleteTarget(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete {deleteTarget?.alias}?</AlertDialogTitle>
            <AlertDialogDescription>
              Threads that already used this model keep their history — only future messages lose
              access to it. This can&apos;t be undone.
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

function ModelFormDialog({
  open,
  onOpenChange,
  editing,
  credentials,
  onCreate,
  onUpdate,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  editing: ModelConfig | null
  credentials: Credential[]
  onCreate: (body: CreateModelRequest) => Promise<ModelConfig>
  onUpdate: (id: Uuid, patch: UpdateModelRequest) => Promise<ModelConfig>
}) {
  // The parent remounts this component (via `key`) each time the dialog
  // opens, so the lazy initializer alone is enough to seed a fresh draft.
  const [form, setForm] = React.useState<FormState>(() => (editing ? formFromModel(editing) : EMPTY_FORM))
  const [submitting, setSubmitting] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault()
    setSubmitting(true)
    setError(null)
    try {
      const body = requestFromForm(form)
      if (editing) {
        await onUpdate(editing.id, body)
      } else {
        await onCreate(body)
      }
      onOpenChange(false)
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to save the model")
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={handleSubmit} className="flex flex-col gap-4">
          <DialogHeader>
            <DialogTitle>{editing ? `Edit ${editing.alias}` : "Add model"}</DialogTitle>
          </DialogHeader>

          <AnimatedField
            id="model-alias"
            label="Alias"
            value={form.alias}
            onChange={(v) => setForm((f) => ({ ...f, alias: v }))}
            placeholder="fast"
            required
            hint="What you'd type as faber -m <alias>."
          />

          <div className="w-full">
            <label htmlFor="model-wire" className="mb-1.5 block text-sm font-medium text-foreground/80">
              Wire<span className="ml-0.5 text-accent-foreground/70">*</span>
            </label>
            <select
              id="model-wire"
              value={form.wire}
              onChange={(e) => setForm((f) => ({ ...f, wire: e.target.value as Wire }))}
              className="w-full rounded-lg border border-border bg-card px-3 py-2.5 text-[15px] text-foreground outline-none transition-colors hover:border-foreground/20 focus:border-primary focus:ring-4 focus:ring-primary/10"
            >
              {WIRE_OPTIONS.map((wire) => (
                <option key={wire} value={wire}>
                  {wire}
                </option>
              ))}
            </select>
          </div>

          <AnimatedField
            id="model-wire-id"
            label="Wire id"
            value={form.wire_id}
            onChange={(v) => setForm((f) => ({ ...f, wire_id: v }))}
            placeholder="claude-opus-5"
            required
            hint="The provider's own id for the model."
          />

          <AnimatedField
            id="model-base-url"
            label="Base URL"
            value={form.base_url}
            onChange={(v) => setForm((f) => ({ ...f, base_url: v }))}
            placeholder={form.wire === "openai" ? "https://api.openai.com/v1" : "https://api.anthropic.com"}
            required
            hint={form.wire === "openai" ? "Include the provider's API prefix, such as /v1." : undefined}
          />

          <AnimatedField
            id="model-family"
            label="Family"
            value={form.family}
            onChange={(v) => setForm((f) => ({ ...f, family: v }))}
            placeholder="Optional"
          />

          <div className="w-full">
            <label
              htmlFor="model-reasoning"
              className="mb-1.5 block text-sm font-medium text-foreground/80"
            >
              Reasoning history
            </label>
            <select
              id="model-reasoning"
              value={form.reasoning_history}
              onChange={(e) =>
                setForm((f) => ({
                  ...f,
                  reasoning_history: e.target.value as ReasoningHistory | "",
                }))
              }
              className="w-full rounded-lg border border-border bg-card px-3 py-2.5 text-[15px] text-foreground outline-none transition-colors hover:border-foreground/20 focus:border-primary focus:ring-4 focus:ring-primary/10"
            >
              {REASONING_OPTIONS.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
            <p className="mt-1.5 text-sm text-muted-foreground">
              What this model gets back when an earlier answer of its own is replayed.
              Some reject reasoning sent without its signature; others reject it entirely.
            </p>
          </div>

          <div className="w-full">
            <label htmlFor="model-credential" className="mb-1.5 block text-sm font-medium text-foreground/80">
              Credential
            </label>
            <select
              id="model-credential"
              value={form.credential_id}
              onChange={(e) => setForm((f) => ({ ...f, credential_id: e.target.value as Uuid | "" }))}
              className="w-full rounded-lg border border-border bg-card px-3 py-2.5 text-[15px] text-foreground outline-none transition-colors hover:border-foreground/20 focus:border-primary focus:ring-4 focus:ring-primary/10"
            >
              <option value="">None</option>
              {credentials.map((credential) => (
                <option key={credential.id} value={credential.id}>
                  {credential.label}
                </option>
              ))}
            </select>
          </div>

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          <DialogFooter>
            <Button type="submit" loading={submitting} loadingText={editing ? "Saving" : "Adding"}>
              {editing ? "Save" : "Add model"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
