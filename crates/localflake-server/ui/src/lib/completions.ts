// Autocomplete for the editor: names from the catalog in the worksheet's context, keywords and
// functions.

import {
  catalog,
  loadDatabase,
  loadDatabases,
  objectOf,
  schemasOf,
} from "@/lib/catalog"
import { FUNCTIONS, KEYWORDS, ident, tableAliases } from "@/lib/sql"
import type { CatalogObject } from "@/lib/types"

export type CompletionKind =
  | "column"
  | "table"
  | "view"
  | "schema"
  | "database"
  | "keyword"
  | "function"

export type Completion = {
  label: string
  /** What's inserted, if not the label (a quoted name, or `FN(`). */
  insert?: string
  /** Shown on the right: a column's type, "table", "keyword"... */
  detail: string
  kind: CompletionKind
  /** Lower ranks list first. */
  rank: number
}

/** The identifier path being typed: `DB.SCH.pre` is `{path: ["DB", "SCH"], prefix: "pre"}`. */
export type Word = {
  path: string[]
  prefix: string
  start: number
  end: number
}

export function wordBefore(text: string, offset: number): Word {
  const before = text.slice(0, offset)
  const match = /((?:(?:"[^"]*"|[A-Za-z_][\w$]*)\.)*)("?[\w$]*)$/.exec(before)
  const dotted = match?.[1] ?? ""
  const prefix = match?.[2] ?? ""
  const path = dotted
    ? (dotted.slice(0, -1).match(/"[^"]*"|[A-Za-z_][\w$]*/g) ?? []).map((p) =>
        p.startsWith('"') ? p.slice(1, -1) : p.toUpperCase()
      )
    : []
  return { path, prefix, start: offset - prefix.length, end: offset }
}

/** Completion candidates for an identifier path in a worksheet's context. */
export async function completions(
  word: Word,
  sql: string,
  offset: number,
  database: string | null,
  schema: string | null
): Promise<Completion[]> {
  await loadDatabases().catch(() => [])
  if (database) await loadDatabase(database).catch(() => null)
  const items: Completion[] = []
  const add = (
    label: string,
    detail: string,
    kind: CompletionKind,
    rank: number
  ) => items.push({ label, insert: ident(label), detail, kind, rank })
  const objectItems = (db: string, sch: string) => {
    const objects =
      schemasOf(catalog.get(), db).find((s) => s.name === sch)?.objects ?? []
    for (const o of objects) {
      add(
        o.name,
        o.kind.replace("_", " ").toLowerCase(),
        o.kind.includes("VIEW") ? "view" : "table",
        1
      )
    }
  }
  const columnItems = (object: CatalogObject | undefined) =>
    object?.columns.forEach((c) => add(c.name, c.type, "column", 0))
  const resolveObject = (parts: string[]) => {
    const [db, sch, name] =
      parts.length === 3
        ? parts
        : parts.length === 2
          ? [database, ...parts]
          : [database, schema, parts[0]]
    if (db && !catalog.get().byDatabase[db]) return undefined
    return objectOf(catalog.get(), db, sch, name ?? "")
  }
  const isDatabase = (name: string) =>
    catalog.get().databases?.some((d) => d.name === name) ?? false
  // The statement the cursor is in, for its tables' aliases.
  const statement = sql.slice(
    Math.max(0, sql.lastIndexOf(";", offset - 1) + 1),
    offset + 2000
  )

  const path = word.path
  if (path.length) {
    // `alias.` or `table.` → columns; `db.` → schemas; `db.schema.` / `schema.` → objects.
    const aliases = tableAliases(statement)
    const last = path[path.length - 1]
    if (path.length === 1 && aliases.has(last)) {
      const parts = aliases.get(last) ?? []
      const db = parts.length === 3 ? parts[0] : database
      if (db) await loadDatabase(db).catch(() => null)
      columnItems(resolveObject(parts))
    }
    if (path.length === 1 && isDatabase(last)) {
      const data = await loadDatabase(last).catch(() => null)
      data?.schemas.forEach((s) => add(s.name, "schema", "schema", 1))
    }
    if (path.length === 1 && database) objectItems(database, last)
    if (path.length === 1 && database && schema) {
      columnItems(objectOf(catalog.get(), database, schema, last))
    }
    if (path.length === 2) {
      if (isDatabase(path[0])) {
        await loadDatabase(path[0]).catch(() => null)
        objectItems(path[0], path[1])
      }
      if (database)
        columnItems(objectOf(catalog.get(), database, path[0], path[1]))
    }
    if (path.length === 3) {
      await loadDatabase(path[0]).catch(() => null)
      columnItems(objectOf(catalog.get(), path[0], path[1], path[2]))
    }
    return items
  }

  // Columns of the tables in this statement, then objects, schemas, databases, keywords and
  // functions.
  for (const parts of new Set(tableAliases(statement).values())) {
    columnItems(resolveObject(parts))
  }
  if (database && schema) objectItems(database, schema)
  if (database) {
    schemasOf(catalog.get(), database).forEach((s) =>
      add(s.name, "schema", "schema", 2)
    )
  }
  catalog
    .get()
    .databases?.forEach((d) => add(d.name, "database", "database", 2))
  for (const keyword of KEYWORDS) {
    items.push({ label: keyword, detail: "keyword", kind: "keyword", rank: 3 })
  }
  for (const fn of FUNCTIONS) {
    items.push({
      label: fn,
      insert: `${fn}(`,
      detail: "function",
      kind: "function",
      rank: 4,
    })
  }
  const seen = new Set<string>()
  return items.filter((item) => {
    const key = `${item.label}|${item.detail}`
    if (seen.has(key)) return false
    seen.add(key)
    return true
  })
}
