/**
 * Turning a host row into what the Environments page is supposed to say.
 *
 * Two rules drive everything here:
 *
 * 1. **Mode is rendered as what it buys**, not as a registration detail. The
 *    difference between direct and docker is a trust posture the user is
 *    choosing — whether the substrate enforces the boundary or the harness is
 *    enforcing it by convention alone — and the UI should say so out loud.
 * 2. **A probe is an observation, never a status.** Everything below phrases
 *    the last probe in the past tense with its age attached. There is no
 *    "online" here to accidentally gate on.
 *
 * Isolation posture is derived at render time and never stored: `(transport,
 * exec_mode)` already determines it, and a second copy would only invite the
 * two to disagree.
 */

import type { ExecMode, Host, HostProbe, Transport } from "@/lib/api"

export type Posture = {
  /** Short label, e.g. "Docker over SSH". */
  title: string
  /** What the user is actually choosing, in one sentence. */
  summary: string
  /** Filesystem reach the agent gets. */
  filesystem: string
  /** Who is enforcing the boundary — the substrate, or convention. */
  enforcement: string
  /** Whether teardown removes anything real. */
  teardown: string
  /** `true` when nothing but the harness stands between the agent and the box. */
  conventionOnly: boolean
}

function transportLabel(transport: Transport): string {
  return transport === "ssh" ? "over SSH" : "on this machine"
}

export function posture(host: Host): Posture {
  const where = transportLabel(host.transport)

  if (host.exec_mode === "docker") {
    return {
      title: `Docker ${where}`,
      summary:
        "The container substrate enforces the boundary — what you take away is actually taken away.",
      filesystem: "Scoped to the container's filesystem and its mounts.",
      enforcement: "Enforced by the container runtime, not by faber.",
      teardown: "Teardown is real: removing the container removes what it held.",
      conventionOnly: false,
    }
  }

  return {
    title: `Direct ${where}`,
    summary:
      host.transport === "ssh"
        ? "Commands run as the SSH user, against that machine's own toolchain and filesystem."
        : "Commands run against this machine's own toolchain and filesystem.",
    filesystem: "The whole filesystem the account can reach.",
    enforcement:
      "Removed capabilities are removed by convention — the harness subtracts them, nothing below it does.",
    teardown: "There is no teardown: changes persist on the machine.",
    conventionOnly: true,
  }
}

/** What the host carries — the image's toolchain, or the user's own. */
export function capabilitySource(execMode: ExecMode): string {
  return execMode === "docker"
    ? "the image's toolchain"
    : "the machine's own toolchain"
}

const MINUTE = 60_000
const HOUR = 60 * MINUTE
const DAY = 24 * HOUR

/** "3h ago" / "just now". Coarse on purpose — this is an age, not a clock. */
export function age(iso: string, now: number = Date.now()): string {
  const elapsed = now - new Date(iso).getTime()
  if (!Number.isFinite(elapsed)) return "at an unknown time"
  if (elapsed < MINUTE) return "just now"
  if (elapsed < HOUR) return `${Math.floor(elapsed / MINUTE)}m ago`
  if (elapsed < DAY) return `${Math.floor(elapsed / HOUR)}h ago`
  return `${Math.floor(elapsed / DAY)}d ago`
}

/**
 * The last probe as a sentence about the past.
 *
 * Never returns anything a caller could read as current state: a host that has
 * never been probed reports exactly that, rather than defaulting to either
 * "up" or "down".
 */
export function observation(probe: HostProbe | null): string {
  if (!probe) return "Never probed"
  if (probe.ok) return `Last reachable ${age(probe.probed_at)}`
  return `Last attempt ${age(probe.probed_at)}: ${probe.error ?? "failed"}`
}

/** The probe's capability manifest as sorted `name version` pairs. */
export function toolList(probe: HostProbe | null): Array<[string, string]> {
  if (!probe?.tools || typeof probe.tools !== "object" || Array.isArray(probe.tools)) {
    return []
  }
  return Object.entries(probe.tools)
    .map(([name, version]) => [name, String(version)] as [string, string])
    .sort(([a], [b]) => a.localeCompare(b))
}

/** How the host is addressed, for the line under its name. */
export function addressLabel(host: Host): string {
  if (host.transport === "ssh") return host.ssh_address ?? "ssh"
  return "local"
}
