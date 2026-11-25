export type Info = {
  version: string
  /** Where databases are stored, or null when they're in memory. */
  dataDir: string | null
  startedAt: number
}

/** A session's context after a request. */
export type Context = {
  sessionId: number
  user: string
  role: string | null
  warehouse: string | null
  database: string | null
  schema: string | null
  timezone: string | null
}

/** A result column, as a driver's `rowtype` describes it. */
export type Column = {
  name: string
  type: string
  nullable?: boolean
  precision?: number | null
  scale?: number | null
  length?: number | null
  fields?: Column[] | null
  vectorDimension?: number | null
}

/** Values in the JSON wire encoding, which the page formats. */
export type Row = (string | null)[]

export type QueryError = {
  code?: string | null
  sqlState?: string | null
  message: string
  queryId?: string | null
}

/** A page of one statement's result. */
export type Result = {
  queryId: string
  kind: string
  statement: string
  columns: Column[]
  rows: Row[]
  total: number
  offset: number
  durationMicros: number | null
  timezone: string | null
  /** A PUT or GET, which moves files between a driver and a stage. */
  transfer: boolean
}

export type QueryResponse = {
  results: Result[]
  error: QueryError | null
  context: Context
}

export type QueryStatus = "running" | "succeeded" | "failed"

export type HistoryRecord = {
  queryId: string
  sequence: number
  sessionId: number
  user: string
  client: string
  database: string | null
  schema: string | null
  queryTag: string | null
  sql: string
  sqlTruncated: boolean
  status: QueryStatus
  kind: string | null
  rows: number | null
  error: QueryError | null
  startedAt: number
  durationMicros: number | null
  // Only in a single record's detail:
  duckdbSql?: string[]
  timezone?: string | null
  resultAvailable?: boolean
}

export type SessionSummary = {
  id: number
  user: string
  client: string
  database: string | null
  schema: string | null
  loggedInAt: number
  lastActiveAt: number
  requests: number
  running: boolean
  webUi: boolean
}

export type Database = {
  name: string
  comment: string | null
  createdOn: string | null
}

export type CatalogColumn = {
  name: string
  type: string
  nullable: boolean
  default: string | null
  autoincrement: string | null
  comment: string | null
}

export type CatalogObject = {
  name: string
  /** TABLE, VIEW, MATERIALIZED_VIEW, DYNAMIC_TABLE... */
  kind: string
  rows: number | null
  bytes: number | null
  comment: string | null
  columns: CatalogColumn[]
}

export type Routine = {
  name: string
  signature: string | null
  description: string | null
}

/** A stage, sequence or file format, with one detail (URL, next value, type). */
export type Named = {
  name: string
  detail: string | null
  comment: string | null
}

export type Schema = {
  name: string
  comment: string | null
  objects: CatalogObject[]
  procedures: Routine[]
  functions: Routine[]
  stages: Named[]
  sequences: Named[]
  fileFormats: Named[]
}

export type DatabaseCatalog = { name: string; schemas: Schema[] }

export type SavedQuery = {
  id: string
  name: string
  sql: string
  database?: string | null
  schema?: string | null
  createdAt: number
  updatedAt: number
}
