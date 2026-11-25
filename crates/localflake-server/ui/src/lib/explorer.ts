// The object explorer's state and the actions its menus run. Every action runs in the active
// worksheet's session, so it shows in the query history too.

import { toast } from "sonner"

import { api, message } from "@/lib/api"
import { loadDatabase, refreshCatalogSoon } from "@/lib/catalog"
import { confirm, prompt } from "@/lib/dialogs"
import { ident, qualified } from "@/lib/sql"
import { load, save } from "@/lib/storage"
import { createStore } from "@/lib/store"
import { activeId, editors, ensureSession, runSql } from "@/lib/worksheets"

/**
 * Expanded nodes, by key: `db:NAME`, `schema:DB.SCHEMA`, `obj:DB.SCHEMA.NAME`. Groups (Tables,
 * Views...) are open unless `group:DB.SCHEMA:Title:closed` is here.
 */
export const expanded = createStore<ReadonlySet<string>>(
  new Set(load<string[]>("expanded", []))
)
expanded.subscribe(() => save("expanded", [...expanded.get()]))

export function toggle(key: string, open?: boolean) {
  expanded.set((current) => {
    const next = new Set(current)
    if (open ?? !current.has(key)) next.add(key)
    else next.delete(key)
    return next
  })
  if (key.startsWith("db:") && expanded.get().has(key)) {
    loadDatabase(key.slice(3)).catch((e: unknown) => toast.error(message(e)))
  }
}

export function insertIntoEditor(text: string) {
  const editor = editors.get(activeId())
  editor?.insert(text)
  editor?.focus()
}

/** Runs a query in the active worksheet without touching its SQL. */
export function preview(sqlOrName: string, isSql = false) {
  const sql = isSql ? sqlOrName : `SELECT * FROM ${sqlOrName} LIMIT 100`
  return runSql(activeId(), sql, { label: sql })
}

export async function dropObject(sql: string, what: string, verb = "Drop") {
  const ok = await confirm({
    title: verb === "Drop" ? `Drop ${what.split(" ")[0]}` : `${verb} table`,
    message: `This permanently removes ${what}. It runs in the active worksheet's session:`,
    sql,
    confirmLabel: verb,
    danger: true,
  })
  if (!ok) return
  const data = await runSql(activeId(), sql, { label: sql })
  if (data?.error) toast.error(data.error.message)
  else if (data) {
    toast(
      `${verb === "Drop" ? "Dropped" : "Truncated"} ${what.replace(/^every row of /, "")}`
    )
  }
  refreshCatalogSoon()
}

export async function createObject(
  kind: "database" | "schema" | "stage",
  database?: string,
  schema?: string
) {
  const titles = {
    database: "Create database",
    schema: `Create schema in ${database}`,
    stage: `Create stage in ${database}.${schema}`,
  }
  const name = await prompt({
    title: titles[kind],
    label: "Name",
    placeholder: { database: "MY_DB", schema: "MY_SCHEMA", stage: "MY_STAGE" }[
      kind
    ],
    help: 'Unquoted names are upper-cased, as in Snowflake. Use "quotes" to keep the case.',
    confirmLabel: "Create",
  })
  if (!name) return
  const target =
    kind === "database"
      ? name
      : kind === "schema"
        ? `${ident(database ?? "")}.${name}`
        : `${qualified(database, schema)}.${name}`
  const sql = `CREATE ${kind.toUpperCase()} ${target}`
  // CREATE DATABASE and CREATE SCHEMA switch the session to it, as in Snowflake.
  const data = await runSql(activeId(), sql, { label: sql })
  if (data?.error) toast.error(data.error.message)
  else if (data) {
    toast(`Created ${kind} ${name}`)
    if (kind === "database")
      toggle(`db:${data.context?.database ?? name}`, true)
    if (kind === "schema") {
      toggle(`schema:${database}.${data.context?.schema}`, true)
    }
  }
  refreshCatalogSoon()
}

/** Uploads a file to a stage, as a driver's PUT would. */
export async function uploadFile(stage: string, file: File) {
  const session = await ensureSession(activeId())
  const query = new URLSearchParams({ session, stage, name: file.name })
  return api<{ size: number }>(`/stage-files?${query}`, {
    method: "PUT",
    raw: file,
  })
}

/** Downloads a file listed by LIST, as a driver's GET would. */
export async function downloadStageFile(stage: string, listedName: string) {
  // LIST shows named stages' files under the stage's lower-case name: drop it.
  let name = listedName
  const stageName = /^@(?:[^/]*\.)?("?)([^/".]+)\1/.exec(stage)?.[2]
  if (
    stageName &&
    !stage.startsWith("@~") &&
    !stage.startsWith("@%") &&
    name.toLowerCase().startsWith(`${stageName.toLowerCase()}/`)
  ) {
    name = name.slice(stageName.length + 1)
  }
  try {
    const session = await ensureSession(activeId())
    const query = new URLSearchParams({
      session,
      stage: stage.split("/")[0],
      name,
    })
    const response = await fetch(`/__localflake/api/stage-files?${query}`)
    if (!response.ok) {
      const data = (await response.json().catch(() => ({}))) as {
        error?: { message?: string }
      }
      throw new Error(data.error?.message ?? response.statusText)
    }
    const url = URL.createObjectURL(await response.blob())
    const a = document.createElement("a")
    a.href = url
    a.download = name.split("/").pop() ?? name
    document.body.append(a)
    a.click()
    a.remove()
    setTimeout(() => URL.revokeObjectURL(url), 1000)
  } catch (e) {
    toast.error(message(e))
  }
}
