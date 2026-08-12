"use client"

import * as React from "react"

import {
  faber,
  FaberError,
  type CreateImageRequest,
  type Image,
  type UpdateImageRequest,
  type Uuid,
} from "@/lib/api"

/**
 * The saved spawn templates.
 *
 * Lifted out of the Images section because two things on `/environments` need
 * the same list: the section that manages it, and the Create dialog that spawns
 * a container from one. Fetching twice would let the two disagree about what
 * exists.
 */
export function useImages() {
  const [images, setImages] = React.useState<Image[]>([])
  const [loaded, setLoaded] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)

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

  const removeImage = React.useCallback(async (id: Uuid): Promise<void> => {
    try {
      await faber.deleteImage(id)
      setImages((prev) => prev.filter((image) => image.id !== id))
    } catch (err) {
      setError(err instanceof FaberError ? err.message : "failed to delete the image")
      throw err
    }
  }, [])

  return { images, loaded, error, addImage, editImage, removeImage }
}
