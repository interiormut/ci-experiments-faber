"use client"

import * as React from "react"
import { Container, HardDrive, Layers, ShieldAlert, ShieldCheck, Trash } from "lucide-react"

import type { Host } from "@/lib/api"
import { useHosts } from "@/lib/hosts/use-hosts"
import {
  addressLabel,
  capabilitySource,
  observation,
  posture,
  toolList,
} from "@/lib/hosts/posture"

/**
 * The read side of the same rows the Hosts page registers: one list, all hosts,
 * containers nested where applicable.
 *
 * Two things this page is careful about, both from `internal-docs/host.md`:
 *
 * * **Mode is rendered as what it buys** — filesystem scope, who enforces the
 *   boundary, whether teardown is real — not as the registration detail
 *   `direct` / `docker`. That difference is a trust posture the user is
 *   choosing, so the page says it in those terms.
 * * **The probe is stated as an observation**, never as status. There is no
 *   green dot here, and nothing on this page should ever grow one: the
 *   authoritative answer to "is it reachable" is the next connection attempt,
 *   and a light would invite reading a cached one instead.
 */
export default function EnvironmentsPage() {
  const { hosts, loaded, error } = useHosts()

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto flex w-full max-w-3xl flex-col gap-6 px-4 py-10">
        <div>
          <h1 className="text-lg font-semibold tracking-tight">Environments</h1>
          <p className="text-sm text-muted-foreground">
            Where the agent runs, and what each choice actually buys you.
          </p>
        </div>

        {error ? <p className="text-sm text-destructive">{error}</p> : null}

        {!loaded ? (
          <p className="text-sm text-muted-foreground">Loading…</p>
        ) : hosts.length === 0 ? (
          <div className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border py-16 text-center">
            <Layers className="h-6 w-6 text-muted-foreground" />
            <div>
              <p className="text-sm font-medium">Nowhere to run yet</p>
              <p className="text-sm text-muted-foreground">
                Register a host to give the agent an environment.
              </p>
            </div>
          </div>
        ) : (
          <ul className="flex flex-col gap-4">
            {hosts.map((host) => (
              <EnvironmentCard key={host.id} host={host} />
            ))}
          </ul>
        )}
      </div>
    </div>
  )
}

function EnvironmentCard({ host }: { host: Host }) {
  const shape = posture(host)
  const tools = toolList(host.last_probe)
  const disabled = !!host.disabled_at

  return (
    <li className="rounded-xl border border-border bg-card">
      <div className="flex flex-col gap-3 px-4 py-4">
        <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1">
          <div className="flex items-center gap-2">
            <h2 className="text-sm font-medium">{host.name}</h2>
            {disabled ? (
              <span className="rounded-full bg-muted px-2 py-0.5 text-[11px] font-medium text-muted-foreground">
                disabled
              </span>
            ) : null}
          </div>
          <span className="text-xs text-muted-foreground">{addressLabel(host)}</span>
        </div>

        <div className="flex items-start gap-2.5">
          {shape.conventionOnly ? (
            <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
          ) : (
            <ShieldCheck className="mt-0.5 h-4 w-4 shrink-0 text-primary" />
          )}
          <div className="min-w-0">
            <p className="text-[13px] font-medium">{shape.title}</p>
            <p className="text-[13px] text-muted-foreground">{shape.summary}</p>
          </div>
        </div>

        <dl className="grid gap-x-6 gap-y-2 rounded-lg bg-muted/40 px-3 py-2.5 text-xs sm:grid-cols-3">
          <div>
            <dt className="flex items-center gap-1.5 font-medium text-foreground/80">
              <HardDrive className="h-3.5 w-3.5" />
              Filesystem
            </dt>
            <dd className="mt-0.5 text-muted-foreground">{shape.filesystem}</dd>
          </div>
          <div>
            <dt className="flex items-center gap-1.5 font-medium text-foreground/80">
              <ShieldCheck className="h-3.5 w-3.5" />
              Enforcement
            </dt>
            <dd className="mt-0.5 text-muted-foreground">{shape.enforcement}</dd>
          </div>
          <div>
            <dt className="flex items-center gap-1.5 font-medium text-foreground/80">
              <Trash className="h-3.5 w-3.5" />
              Teardown
            </dt>
            <dd className="mt-0.5 text-muted-foreground">{shape.teardown}</dd>
          </div>
        </dl>

        {/* Stated in the past tense, with its age, because that is all it is.
            "Never probed" is its own answer — not a default to "down". */}
        <p className="text-xs text-muted-foreground">
          {observation(host.last_probe)}
        </p>

        {tools.length > 0 ? (
          <div className="flex flex-wrap items-center gap-1.5">
            <span className="text-xs text-muted-foreground">
              Seen on that attempt:
            </span>
            {tools.map(([name, version]) => (
              <span
                key={name}
                className="rounded-md bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground"
              >
                {name} {version}
              </span>
            ))}
          </div>
        ) : (
          <p className="text-xs text-muted-foreground">
            Capabilities come from {capabilitySource(host.exec_mode)} and are
            discovered when faber connects — not assumed from the mode.
          </p>
        )}
      </div>

      {host.exec_mode === "docker" ? (
        <div className="border-t border-border px-4 py-3">
          <span className="text-[10.5px] font-medium uppercase tracking-[0.12em] text-muted-foreground/70">
            Containers
          </span>
          {host.containers.length === 0 ? (
            <p className="mt-1.5 text-xs text-muted-foreground">
              None registered on this host.
            </p>
          ) : (
            <ul className="mt-1.5 flex flex-col gap-1">
              {host.containers.map((container) => (
                <li key={container.id} className="flex items-center gap-2 text-[13px]">
                  <Container className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                  <span className="truncate">{container.name ?? container.container_ref}</span>
                  <span className="truncate font-mono text-[11px] text-muted-foreground">
                    {container.root_path}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : null}
    </li>
  )
}
