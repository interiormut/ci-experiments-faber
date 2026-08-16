"use client"

import * as React from "react"

import {
  faber,
  type CreateServiceHostRequest,
  type CreateServiceImageRequest,
  type GrantRequest,
  type ServiceHost,
  type ServiceImage,
  type Tenant,
  type UpdateServiceHostRequest,
  type UpdateServiceImageRequest,
  type Uuid,
} from "@/lib/api"

/**
 * The machines faber operates, their tenants, and the templates they run.
 *
 * Fetched per page rather than kept in the app shell: this is a surface most
 * callers never see, and loading it app-wide would spend a 403 on every
 * navigation for everyone who is not an administrator.
 *
 * Tenants are loaded per host and only when a host is expanded. The list is
 * bounded — a tenant exists only once someone has launched something — but it
 * carries a live read of the machine per row, so asking for every host's
 * tenants up front would read the filesystem once per user before anything is
 * on screen.
 */
export function useServiceHosts() {
  const [hosts, setHosts] = React.useState<ServiceHost[]>([])
  const [images, setImages] = React.useState<ServiceImage[]>([])
  const [tenants, setTenants] = React.useState<Record<Uuid, Tenant[]>>({})
  const [loaded, setLoaded] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

  React.useEffect(() => {
    let cancelled = false

    void Promise.all([faber.listServiceHosts(), faber.listServiceImages()])
      .then(([hostRows, imageRows]) => {
        if (cancelled) return
        setHosts(hostRows)
        setImages(imageRows)
      })
      .catch(() => {
        // An empty list and a failed fetch look identical on screen, so say
        // which one happened.
        if (!cancelled) setError("Could not load faber's hosts.")
      })
      .finally(() => {
        if (!cancelled) setLoaded(true)
      })

    return () => {
      cancelled = true
    }
  }, [])

  const loadTenants = React.useCallback(async (hostId: Uuid): Promise<Tenant[]> => {
    const rows = await faber.listTenants(hostId)
    setTenants((prev) => ({ ...prev, [hostId]: rows }))
    return rows
  }, [])

  const addHost = React.useCallback(
    async (body: CreateServiceHostRequest): Promise<ServiceHost> => {
      const created = await faber.createServiceHost(body)
      setHosts((prev) => [...prev, created])
      return created
    },
    [],
  )

  const editHost = React.useCallback(
    async (id: Uuid, patch: UpdateServiceHostRequest): Promise<ServiceHost> => {
      const updated = await faber.updateServiceHost(id, patch)
      setHosts((prev) => prev.map((host) => (host.id === id ? updated : host)))
      return updated
    },
    [],
  )

  const removeHost = React.useCallback(async (id: Uuid): Promise<void> => {
    await faber.deleteServiceHost(id)
    setHosts((prev) => prev.filter((host) => host.id !== id))
  }, [])

  /**
   * Reloads the hosts alongside one host's tenants.
   *
   * Both halves, because a storage grant moves what the host has committed as
   * well as what the tenant is allowed — and a stale committed figure is how
   * an operator talks themselves into a second grant the machine cannot hold.
   */
  const refreshHost = React.useCallback(async (hostId: Uuid): Promise<void> => {
    const [hostRows, tenantRows] = await Promise.all([
      faber.listServiceHosts(),
      faber.listTenants(hostId),
    ])
    setHosts(hostRows)
    setTenants((prev) => ({ ...prev, [hostId]: tenantRows }))
  }, [])

  const grant = React.useCallback(
    async (hostId: Uuid, userId: Uuid, body: GrantRequest): Promise<void> => {
      await faber.grantQuota(hostId, userId, body)
      await refreshHost(hostId)
    },
    [refreshHost],
  )

  const revoke = React.useCallback(
    async (hostId: Uuid, userId: Uuid): Promise<void> => {
      await faber.revokeQuota(hostId, userId)
      await refreshHost(hostId)
    },
    [refreshHost],
  )

  const addImage = React.useCallback(
    async (body: CreateServiceImageRequest): Promise<ServiceImage> => {
      const created = await faber.createServiceImage(body)
      setImages((prev) => [...prev, created])
      return created
    },
    [],
  )

  const editImage = React.useCallback(
    async (id: Uuid, patch: UpdateServiceImageRequest): Promise<ServiceImage> => {
      const updated = await faber.updateServiceImage(id, patch)
      setImages((prev) => prev.map((image) => (image.id === id ? updated : image)))
      return updated
    },
    [],
  )

  const removeImage = React.useCallback(async (id: Uuid): Promise<void> => {
    await faber.deleteServiceImage(id)
    setImages((prev) => prev.filter((image) => image.id !== id))
  }, [])

  return {
    hosts,
    images,
    tenants,
    loaded,
    error,
    loadTenants,
    addHost,
    editHost,
    removeHost,
    grant,
    revoke,
    addImage,
    editImage,
    removeImage,
  }
}
