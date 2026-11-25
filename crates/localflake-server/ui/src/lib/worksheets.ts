// Worksheets: tabs of SQL, each with its own session. They live outside React so a query keeps
// running, and its results stay, while another page shows.

import { toast } from "sonner"

import { ApiError, api, message } from "@/lib/api"
import { refreshCatalogSoon } from "@/lib/catalog"
import { confirm, prompt } from "@/lib/dialogs"
import { MOD, debounce, uid } from "@/lib/format"
import { saved, upsertSaved } from "@/lib/saved"
import { ident, qualified } from "@/lib/sql"
import { load, loadSession, save, saveSession } from "@/lib/storage"
import { createStore } from "@/lib/store"
import type {
  Context,
  QueryError,
  QueryResponse,
  Result,
  SavedQuery,
} from "@/lib/types"

/** What's kept in the browser between visits. */
export type WorksheetModel = {
  id: string
  name: string
  sql: string
  database: string | null
  schema: string | null
  /** The saved query this worksheet shows, if any. */
  savedId?: string | null
  /** The last run's results, shown again after a reload while localflake keeps them. */
  lastQueryIds?: string[]
  lastError?: QueryError | null
}

export type Running = {
  requestId: string
  started: number
  sql: string
  /** Shown instead of "Running…", for the explorer's actions. */
  label?: string
}

export type Worksheet = {
  model: WorksheetModel
  /** The session's context after its last request; null before the first. */
  context: Context | null
  running: Running | null
  results: Result[]
  error: QueryError | null
  /** The result showing: an index into `results`, or `results.length` for the error. */
  activeResult: number
  /** The saved query's SQL when it was opened or last saved, to tell whether it's changed. */
  savedSql: string | null
}

type State = { list: Worksheet[]; activeId: string }

export const STARTER_SQL = `-- Welcome to localflake. Run a statement with ${MOD}+Enter.
SELECT CURRENT_DATABASE(), CURRENT_SCHEMA(), CURRENT_USER(), CURRENT_TIMESTAMP();
`

function fromModel(model: WorksheetModel): Worksheet {
  const query = model.savedId
    ? saved.get().find((q) => q.id === model.savedId)
    : undefined
  return {
    model,
    context: null,
    running: null,
    results: [],
    error: null,
    activeResult: 0,
    savedSql: query?.sql ?? null,
  }
}

function initialState(): State {
  const state = load<{ tabs?: WorksheetModel[]; active?: string } | null>(
    "worksheets",
    null
  )
  const models = state?.tabs?.length
    ? state.tabs
    : [
        {
          id: uid(),
          name: "Worksheet 1",
          sql: STARTER_SQL,
          database: null,
          schema: null,
        },
      ]
  const list = models.map(fromModel)
  const activeId =
    state?.active && list.some((w) => w.model.id === state.active)
      ? state.active
      : list[0].model.id
  return { list, activeId }
}

export const worksheets = createStore<State>(initialState())

function persist() {
  const { list, activeId } = worksheets.get()
  save("worksheets", { tabs: list.map((w) => w.model), active: activeId })
}

export const getWorksheet = (id: string) =>
  worksheets.get().list.find((w) => w.model.id === id)

export const activeId = () => worksheets.get().activeId

export const isDirty = (ws: Worksheet) =>
  ws.model.savedId != null &&
  ws.savedSql != null &&
  ws.savedSql !== ws.model.sql

function update(id: string, change: (ws: Worksheet) => Worksheet) {
  worksheets.set((s) => ({
    ...s,
    list: s.list.map((w) => (w.model.id === id ? change(w) : w)),
  }))
}

const updateModel = (id: string, patch: Partial<WorksheetModel>) =>
  update(id, (w) => ({ ...w, model: { ...w.model, ...patch } }))

// The editors on screen, so actions elsewhere (the explorer's, say) can insert text or read the
// selection.

export type EditorHandle = {
  focus(): void
  focused(): boolean
  /** Replaces the selection, keeping the browser's undo history. */
  insert(text: string): void
  selection(): { start: number; end: number; text: string }
}

export const editors = new Map<string, EditorHandle>()

// Sessions: a worksheet's session token is kept for this browser tab's lifetime, so a reload
// reuses it.

type SessionRef = { token: string; id: number }
const sessionRefs = loadSession<Record<string, SessionRef>>("sessions", {})

function setSessionRef(id: string, ref: SessionRef | null) {
  if (ref) sessionRefs[id] = ref
  else delete sessionRefs[id]
  saveSession("sessions", sessionRefs)
}

export const sessionToken = (id: string) => sessionRefs[id]?.token ?? null

export async function ensureSession(id: string) {
  const ref = sessionRefs[id]
  if (ref) return ref.token
  const ws = getWorksheet(id)
  const data = await api<{ session: string; context: Context }>("/sessions", {
    method: "POST",
    body: { database: ws?.model.database, schema: ws?.model.schema },
  })
  setSessionRef(id, { token: data.session, id: data.context.sessionId })
  update(id, (w) => ({ ...w, context: data.context }))
  return data.session
}

/** Ends a worksheet's session, if it has one. */
function endSession(id: string) {
  const sessionId = getWorksheet(id)?.context?.sessionId ?? sessionRefs[id]?.id
  if (sessionId != null) {
    api(`/sessions/${sessionId}`, { method: "DELETE" }).catch(() => undefined)
  }
  setSessionRef(id, null)
  update(id, (w) => ({ ...w, context: null }))
}

/** After a session is ended elsewhere: its worksheet starts a new one when it next runs. */
export function forgetSession(sessionId: number) {
  for (const [id, ref] of Object.entries(sessionRefs)) {
    if (ref.id === sessionId) setSessionRef(id, null)
  }
}

/** Runs SQL in a worksheet's session, starting a new session if it ended. */
async function execute(id: string, sql: string, requestId?: string) {
  for (let attempt = 0; ; attempt++) {
    const session = await ensureSession(id)
    try {
      return await api<QueryResponse>("/query", {
        method: "POST",
        body: { session, sql, requestId },
      })
    } catch (e) {
      if (e instanceof ApiError && e.code === "390111" && attempt === 0) {
        setSessionRef(id, null)
        continue
      }
      throw e
    }
  }
}

// Tabs -------------------------------------------------------------------------------------------

export function activate(id: string) {
  worksheets.set((s) => ({ ...s, activeId: id }))
  setTimeout(() => editors.get(id)?.focus())
}

export function createWorksheet({
  name,
  sql = "",
  database,
  schema,
  savedId = null,
}: {
  name?: string
  sql?: string
  database?: string | null
  schema?: string | null
  savedId?: string | null
} = {}) {
  const current = getWorksheet(activeId())
  const model: WorksheetModel = {
    id: uid(),
    name: name || `Worksheet ${worksheets.get().list.length + 1}`,
    sql,
    database:
      database !== undefined ? database : (current?.model.database ?? null),
    schema: schema !== undefined ? schema : (current?.model.schema ?? null),
    savedId,
  }
  const ws = fromModel(model)
  worksheets.set((s) => ({ list: [...s.list, ws], activeId: model.id }))
  setTimeout(() => editors.get(model.id)?.focus())
  return model.id
}

export async function closeWorksheet(id: string) {
  const ws = getWorksheet(id)
  if (!ws) return
  if (
    isDirty(ws) &&
    !(await confirm({
      title: "Close worksheet",
      message: `“${ws.model.name}” has changes that aren't in its saved query. Close it anyway?`,
      confirmLabel: "Close",
      danger: true,
    }))
  ) {
    return
  }
  endSession(id)
  ranges.delete(id)
  worksheets.set((s) => {
    const index = s.list.findIndex((w) => w.model.id === id)
    let list = s.list.filter((w) => w.model.id !== id)
    if (!list.length) {
      list = [
        fromModel({
          id: uid(),
          name: "Worksheet 1",
          sql: "",
          database: ws.model.database,
          schema: ws.model.schema,
        }),
      ]
    }
    const next =
      s.activeId === id
        ? list[Math.min(index, list.length - 1)].model.id
        : s.activeId
    return { list, activeId: next }
  })
}

export function renameWorksheet(id: string, name: string) {
  const ws = getWorksheet(id)
  const trimmed = name.trim()
  if (!ws || !trimmed) return
  updateModel(id, { name: trimmed })
  const savedId = ws.model.savedId
  if (savedId && saved.get().some((q) => q.id === savedId)) {
    upsertSaved({ id: savedId, name: trimmed })
  }
}

export function setSql(id: string, sql: string) {
  updateModel(id, { sql })
}

export function setActiveResult(id: string, index: number) {
  update(id, (w) => ({ ...w, activeResult: index }))
}

// Context ----------------------------------------------------------------------------------------

function applyContext(id: string, context: Context) {
  update(id, (w) => ({
    ...w,
    context,
    model: { ...w.model, database: context.database, schema: context.schema },
  }))
}

/** Switches a worksheet's database and schema, as USE would. */
export async function switchContext(
  id: string,
  database: string | null,
  schema: string | null
) {
  if (!database) {
    // There's no USE for "no database": start a fresh session without one.
    endSession(id)
    updateModel(id, { database: null, schema: null })
    return
  }
  const sql = schema
    ? `USE SCHEMA ${qualified(database, schema)}`
    : `USE DATABASE ${ident(database)}`
  try {
    const data = await execute(id, sql)
    if (data.error) toast.error(data.error.message)
    if (data.context) applyContext(id, data.context)
  } catch (e) {
    toast.error(message(e))
  }
}

// Running ----------------------------------------------------------------------------------------

type Range = [number, number]
const ranges = new Map<string, { sql: string; ranges: Range[] }>()

/** Where each statement is, or an error if the SQL can't be split (an unclosed quote, say). */
export async function statementRanges(id: string, sql: string) {
  const cached = ranges.get(id)
  if (cached?.sql === sql) return cached.ranges
  const data = await api<{ ranges: Range[]; error?: string }>("/split", {
    method: "POST",
    body: { sql },
  })
  if (data.error && !data.ranges.length) throw new Error(data.error)
  ranges.set(id, { sql, ranges: data.ranges })
  return data.ranges
}

/** The statement at an offset: the one containing it, else the nearest one before it. */
export function statementAt(list: Range[], offset: number) {
  let best: Range | null = null
  for (const range of list) {
    if (offset >= range[0] && offset <= range[1] + 1) return range
    if (range[1] < offset) best = range
  }
  return best ?? list[0] ?? null
}

/** Runs the selection, else the statement at the cursor (or every statement). */
export async function run(id: string, mode: "cursor" | "all") {
  const ws = getWorksheet(id)
  const editor = editors.get(id)
  if (!ws || ws.running || !editor) return
  const { text: selected, start } = editor.selection()
  const text = ws.model.sql
  let sql = text
  if (selected.trim()) sql = selected
  else if (mode === "cursor") {
    try {
      const range = statementAt(await statementRanges(id, text), start)
      if (range) sql = text.slice(range[0], range[1])
    } catch (e) {
      // Running everything instead could run statements the cursor isn't on.
      if (!/empty/i.test(message(e))) {
        toast.error(
          `Can't find the statement at the cursor: ${message(e)}. Select the SQL to run it.`
        )
        return
      }
    }
  }
  if (!sql.trim()) {
    toast("Nothing to run")
    return
  }
  await runSql(id, sql)
}

/** Runs SQL and shows its results (the explorer's actions use this too). */
export async function runSql(
  id: string,
  sql: string,
  { label }: { label?: string } = {}
): Promise<QueryResponse | null> {
  const ws = getWorksheet(id)
  if (!ws) return null
  if (ws.running) {
    toast.error("A query is already running in this worksheet")
    return null
  }
  const requestId = uid()
  update(id, (w) => ({
    ...w,
    running: { requestId, started: performance.now(), sql, label },
  }))
  let data: QueryResponse | null = null
  let results: Result[] = []
  let error: QueryError | null = null
  try {
    data = await execute(id, sql, requestId)
    results = data.results
    error = data.error
  } catch (e) {
    error = { message: message(e) }
  }
  const context = data?.context
  update(id, (w) => ({
    ...w,
    running: null,
    results,
    error,
    activeResult: error ? results.length : Math.max(0, results.length - 1),
    context: context ?? w.context,
    model: {
      ...w.model,
      lastQueryIds: results.map((r) => r.queryId),
      lastError: error,
      ...(context
        ? { database: context.database, schema: context.schema }
        : {}),
    },
  }))
  if (results.some((r) => !["query", "show"].includes(r.kind))) {
    refreshCatalogSoon()
  }
  return data
}

export async function cancel(id: string) {
  const running = getWorksheet(id)?.running
  const session = sessionToken(id)
  if (!running || !session) return
  try {
    await api("/cancel", {
      method: "POST",
      body: { session, requestId: running.requestId },
    })
  } catch (e) {
    toast.error(message(e))
  }
}

// Saved queries ----------------------------------------------------------------------------------

/** Opens a saved query, in the worksheet already showing it if there is one. */
export function openSaved(query: SavedQuery) {
  const { list } = worksheets.get()
  const open = list.find((w) => w.model.savedId === query.id)
  if (open) {
    activate(open.model.id)
    return
  }
  const current = getWorksheet(activeId())
  if (current && !current.model.sql.trim() && !current.model.savedId) {
    update(current.model.id, (w) => ({
      ...w,
      savedSql: query.sql,
      model: {
        ...w.model,
        savedId: query.id,
        name: query.name,
        sql: query.sql,
      },
    }))
    if (query.database) {
      void switchContext(current.model.id, query.database, query.schema ?? null)
    }
    return
  }
  const id = createWorksheet({
    name: query.name,
    sql: query.sql,
    database: query.database ?? undefined,
    schema: query.schema ?? undefined,
    savedId: query.id,
  })
  update(id, (w) => ({ ...w, savedSql: query.sql }))
}

/** Asks for a name and saves the worksheet's SQL as a saved query. */
export async function saveWorksheet(
  id: string,
  { rename = false }: { rename?: boolean } = {}
) {
  const ws = getWorksheet(id)
  if (!ws) return
  const existing = ws.model.savedId
    ? saved.get().find((q) => q.id === ws.model.savedId)
    : undefined
  let name = existing?.name ?? null
  if (!existing || rename) {
    const firstComment = /^\s*--\s*(.+)/.exec(ws.model.sql)?.[1]
    name = await prompt({
      title: existing ? "Rename saved query" : "Save query",
      label: "Name",
      value:
        existing?.name ||
        (ws.model.name.startsWith("Worksheet")
          ? (firstComment ?? "")
          : ws.model.name),
      placeholder: "e.g. Orders by region",
      help: "Saved queries are kept in this browser.",
      confirmLabel: "Save",
    })
    if (!name) return
  }
  const current = getWorksheet(id)
  if (!current || !name) return
  const query = upsertSaved({
    id: existing?.id,
    name,
    sql: current.model.sql,
    database: current.model.database,
    schema: current.model.schema,
  })
  update(id, (w) => ({
    ...w,
    savedSql: query.sql,
    model: { ...w.model, savedId: query.id, name: query.name },
  }))
  toast(`Saved “${query.name}”`)
}

/** After a saved query is renamed or deleted in the sidebar. */
export function syncSaved(savedId: string, change: { name: string } | null) {
  worksheets.set((s) => ({
    ...s,
    list: s.list.map((w) =>
      w.model.savedId !== savedId
        ? w
        : change
          ? { ...w, model: { ...w.model, name: change.name } }
          : { ...w, savedSql: null, model: { ...w.model, savedId: null } }
    ),
  }))
}

// Start-up ---------------------------------------------------------------------------------------

/** Shows a worksheet's last results after a reload, if localflake still has them. */
async function restoreResults(id: string) {
  const ws = getWorksheet(id)
  const ids = ws?.model.lastQueryIds ?? []
  if (!ws || (!ids.length && !ws.model.lastError)) return
  const results: Result[] = []
  for (const queryId of ids) {
    try {
      const data = await api<Result & { error?: QueryError }>(
        `/results/${encodeURIComponent(queryId)}`
      )
      if (data.error) break
      results.push(data)
    } catch {
      break
    }
  }
  const now = getWorksheet(id)
  if (!now || now.running || now.results.length) return
  const error =
    results.length === ids.length ? (now.model.lastError ?? null) : null
  update(id, (w) => ({
    ...w,
    results,
    error,
    activeResult: error ? results.length : Math.max(0, results.length - 1),
  }))
}

export function initWorksheets() {
  worksheets.subscribe(debounce(persist, 400))
  addEventListener("beforeunload", persist)
  for (const ws of worksheets.get().list) void restoreResults(ws.model.id)
}
