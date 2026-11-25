// Databases, schemas and their objects, loaded lazily for the explorer, the worksheets' context
// pickers and autocomplete.

import { api } from "@/lib/api"
import { debounce } from "@/lib/format"
import { createStore } from "@/lib/store"
import type { Database, DatabaseCatalog, QueryError } from "@/lib/types"

type CatalogState = {
  /** Null until the list first loads. */
  databases: Database[] | null
  /** The databases loaded so far, by name. */
  byDatabase: Readonly<Record<string, DatabaseCatalog>>
  error: string | null
}

export const catalog = createStore<CatalogState>({
  databases: null,
  byDatabase: {},
  error: null,
})

const pending = new Map<string, Promise<DatabaseCatalog>>()

export async function loadDatabases(force = false) {
  const current = catalog.get().databases
  if (current && !force) return current
  const data = await api<{ databases?: Database[]; error?: QueryError }>(
    "/catalog"
  )
  if (data.error) throw new Error(data.error.message)
  const databases = data.databases ?? []
  catalog.set((s) => {
    // Forget databases that are gone.
    const byDatabase = Object.fromEntries(
      Object.entries(s.byDatabase).filter(([name]) =>
        databases.some((d) => d.name === name)
      )
    )
    return { databases, byDatabase, error: null }
  })
  return databases
}

export function loadDatabase(name: string, force = false) {
  const loaded = catalog.get().byDatabase[name]
  if (loaded && !force) return Promise.resolve(loaded)
  const inFlight = pending.get(name)
  if (inFlight && !force) return inFlight
  const promise = api<DatabaseCatalog & { error?: QueryError }>(
    `/catalog/${encodeURIComponent(name)}`
  )
    .then((data) => {
      if (data.error) throw new Error(data.error.message)
      catalog.set((s) => ({
        ...s,
        byDatabase: { ...s.byDatabase, [name]: data },
      }))
      return data
    })
    .finally(() => pending.delete(name))
  pending.set(name, promise)
  return promise
}

/** Reloads the database list and every database loaded so far. */
export async function refreshCatalog() {
  try {
    await loadDatabases(true)
    await Promise.all(
      Object.keys(catalog.get().byDatabase).map((db) =>
        loadDatabase(db, true).catch(() => undefined)
      )
    )
  } catch (e) {
    catalog.set((s) => ({
      ...s,
      error: e instanceof Error ? e.message : String(e),
    }))
  }
}

/** After a statement that may have changed the catalog. */
export const refreshCatalogSoon = debounce(() => void refreshCatalog(), 300)

export const schemasOf = (state: CatalogState, database: string) =>
  state.byDatabase[database]?.schemas ?? []

export const objectOf = (
  state: CatalogState,
  database: string | null | undefined,
  schema: string | null | undefined,
  name: string
) =>
  database && schema
    ? schemasOf(state, database)
        .find((s) => s.name === schema)
        ?.objects.find((o) => o.name === name)
    : undefined
