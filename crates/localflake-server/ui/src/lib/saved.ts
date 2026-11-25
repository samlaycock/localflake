// Saved queries, kept in this browser.

import { plural, uid } from "@/lib/format"
import { load, save } from "@/lib/storage"
import { createStore } from "@/lib/store"
import type { SavedQuery } from "@/lib/types"

export const saved = createStore<SavedQuery[]>(load("saved", []))
saved.subscribe(() => save("saved", saved.get()))

/** Adds a saved query, or updates the one with `id`. */
export function upsertSaved(
  query: Partial<SavedQuery> & { id?: string | null }
): SavedQuery {
  const now = Date.now()
  const existing = query.id
    ? saved.get().find((q) => q.id === query.id)
    : undefined
  if (existing) {
    const updated = { ...existing, ...query, id: existing.id, updatedAt: now }
    saved.set((items) => items.map((q) => (q.id === existing.id ? updated : q)))
    return updated
  }
  const created: SavedQuery = {
    name: "",
    sql: "",
    ...query,
    id: uid(),
    createdAt: now,
    updatedAt: now,
  }
  saved.set((items) => [created, ...items])
  return created
}

export function removeSaved(id: string) {
  saved.set((items) => items.filter((q) => q.id !== id))
}

/** Adds the saved queries in an exported JSON file. Returns a summary. */
export async function importSaved(file: File) {
  const items: unknown = JSON.parse(await file.text())
  if (!Array.isArray(items)) {
    throw new Error("expected a JSON array of saved queries")
  }
  let added = 0
  const next = [...saved.get()]
  for (const item of items as Partial<SavedQuery>[]) {
    if (typeof item?.sql !== "string" || typeof item?.name !== "string") {
      continue
    }
    const index = item.id ? next.findIndex((q) => q.id === item.id) : -1
    const now = Date.now()
    if (index >= 0) next[index] = { ...next[index], ...item, updatedAt: now }
    else {
      next.unshift({
        ...item,
        name: item.name,
        sql: item.sql,
        id: item.id || uid(),
        createdAt: item.createdAt || now,
        updatedAt: item.updatedAt || now,
      })
    }
    added++
  }
  saved.set(next)
  return `Imported ${plural(added, "saved query", "saved queries")}`
}
