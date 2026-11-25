import {
  CopyIcon,
  ExternalLinkIcon,
  PauseIcon,
  PlayIcon,
  SearchIcon,
  SquareIcon,
  Trash2Icon,
  XIcon,
} from "lucide-react"
import * as React from "react"
import { useDefaultLayout } from "react-resizable-panels"
import {
  Link,
  useLoaderData,
  useLocation,
  useNavigate,
  useOutlet,
  useOutletContext,
  useParams,
  useRevalidator,
  useSearchParams,
  type LoaderFunctionArgs,
} from "react-router"
import { toast } from "sonner"

import { QueryErrorView } from "@/components/query-error"
import { copy } from "@/components/result-grid"
import { ResultPanel } from "@/components/result-panel"
import { SqlCode } from "@/components/sql-code"
import { StatusBadge } from "@/components/status-badge"
import { Badge } from "@/components/ui/badge"
import { Button, buttonVariants } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@/components/ui/empty"
import {
  InputGroup,
  InputGroupAddon,
  InputGroupInput,
} from "@/components/ui/input-group"
import { Label } from "@/components/ui/label"
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from "@/components/ui/resizable"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Spinner } from "@/components/ui/spinner"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { ApiError, api, message } from "@/lib/api"
import { confirm } from "@/lib/dialogs"
import { formatDuration, plural, timeOfDay } from "@/lib/format"
import { clearHistory, history } from "@/lib/live"
import { layoutStorage } from "@/lib/storage"
import { useStore } from "@/lib/store"
import type { HistoryRecord, QueryError, Result } from "@/lib/types"
import { useUrlInput } from "@/lib/url-input"
import { cn } from "@/lib/utils"
import { createWorksheet } from "@/lib/worksheets"

const STATUSES = [
  { label: "Any status", value: "" },
  { label: "Running", value: "running" },
  { label: "Succeeded", value: "succeeded" },
  { label: "Failed", value: "failed" },
]

const isUi = (r: HistoryRecord) => r.client.startsWith("localflake web UI")

type DetailContext = { tab: string; setTab: (tab: string) => void }

/** Every statement from every client, newest first, updated live. */
export function HistoryView() {
  const live = useStore(history, (s) => s.records)
  const [params, setParams] = useSearchParams()
  const { queryId } = useParams()
  const navigate = useNavigate()
  const [text, setText] = useUrlInput("q")
  const status = params.get("status") ?? ""
  const session = params.get("session") ?? ""
  const hideUi = params.get("hideUi") === "1"
  const [paused, setPaused] = React.useState<ReadonlyMap<
    string,
    HistoryRecord
  > | null>(null)
  const [limit, setLimit] = React.useState(500)
  const [tab, setTab] = React.useState("sql")
  const outlet = useOutlet({ tab, setTab } satisfies DetailContext)
  const layout = useDefaultLayout({
    id: "history-layout",
    storage: layoutStorage,
  })
  const records = paused ?? live

  // Running statements' durations tick.
  const [now, setNow] = React.useState(() => Date.now())
  const anyRunning = [...records.values()].some((r) => r.status === "running")
  React.useEffect(() => {
    if (!anyRunning || paused) return
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [anyRunning, paused])

  const setParam = (name: string, value: string) =>
    setParams(
      (current) => {
        const next = new URLSearchParams(current)
        if (value) next.set(name, value)
        else next.delete(name)
        return next
      },
      { replace: true }
    )

  const needle = text.trim().toLowerCase()
  const filtered = [...records.values()]
    .reverse()
    .filter(
      (r) =>
        (!status || r.status === status) &&
        (!session || String(r.sessionId) === session) &&
        (!hideUi || !isUi(r)) &&
        (!needle ||
          r.sql.toLowerCase().includes(needle) ||
          r.queryId.includes(needle) ||
          (r.error?.message ?? "").toLowerCase().includes(needle) ||
          (r.queryTag ?? "").toLowerCase().includes(needle))
    )
  const shown = filtered.slice(0, limit)

  const sessionItems = [
    { label: "Every session", value: "" },
    ...[
      ...new Map(
        [...records.values()].map((r) => [
          String(r.sessionId),
          `#${r.sessionId} ${r.client || "unknown client"} (${r.user})`,
        ])
      ),
    ]
      .sort((a, b) => Number(b[0]) - Number(a[0]))
      .map(([value, label]) => ({ value, label })),
  ]
  if (session && !sessionItems.some((i) => i.value === session)) {
    sessionItems.push({ value: session, label: `#${session}` })
  }

  async function clear() {
    const ok = await confirm({
      title: "Clear history",
      message:
        "Forget every finished statement in the query history? Stored results stay available to RESULT_SCAN.",
      confirmLabel: "Clear",
      danger: true,
    })
    if (!ok) return
    try {
      await clearHistory()
      setPaused(null)
      if (queryId)
        void navigate({ pathname: "/history", search: params.toString() })
    } catch (e) {
      toast.error(message(e))
    }
  }

  const list = (
    <div className="h-full overflow-auto [&>[data-slot=table-container]]:overflow-visible">
      <Table>
        <TableHeader className="sticky top-0 z-10 bg-background shadow-[0_1px_0_var(--color-border)] [&_tr]:border-0">
          <TableRow className="hover:bg-transparent">
            <TableHead className="w-28" />
            <TableHead>Started</TableHead>
            <TableHead className="text-right">Duration</TableHead>
            <TableHead className="text-right">Rows</TableHead>
            <TableHead>Session</TableHead>
            <TableHead>Context</TableHead>
            <TableHead>SQL</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {shown.map((r) => (
            <TableRow
              key={r.queryId}
              data-state={r.queryId === queryId ? "selected" : undefined}
              className="cursor-pointer"
              onClick={() =>
                void navigate({
                  pathname: `/history/${encodeURIComponent(r.queryId)}`,
                  search: params.toString(),
                })
              }
            >
              <TableCell>
                <StatusBadge status={r.status} />
              </TableCell>
              <TableCell
                className="font-mono text-xs"
                title={new Date(r.startedAt).toLocaleString()}
              >
                {timeOfDay(r.startedAt)}
              </TableCell>
              <TableCell className="text-right text-xs tabular-nums">
                {r.status === "running" ? (
                  <span className="text-muted-foreground">
                    {formatDuration(Math.max(0, now - r.startedAt) * 1000)}
                  </span>
                ) : (
                  formatDuration(r.durationMicros)
                )}
              </TableCell>
              <TableCell className="text-right text-xs tabular-nums">
                {r.rows ?? ""}
              </TableCell>
              <TableCell
                className="max-w-56 truncate text-xs"
                title={`${r.client} · user ${r.user}`}
              >
                <Badge
                  variant={isUi(r) ? "default" : "outline"}
                  className="mr-1.5 font-mono"
                >
                  #{r.sessionId}
                </Badge>
                {r.client || "unknown client"}
              </TableCell>
              <TableCell className="font-mono text-xs text-muted-foreground">
                {[r.database, r.schema].filter(Boolean).join(".")}
              </TableCell>
              <TableCell
                className="max-w-xl truncate font-mono text-xs"
                title={r.sql.slice(0, 2000)}
              >
                {r.queryTag && (
                  <Badge variant="secondary" className="mr-1.5">
                    {r.queryTag}
                  </Badge>
                )}
                {r.sql.replace(/\s+/g, " ").slice(0, 400)}
              </TableCell>
            </TableRow>
          ))}
          {filtered.length > shown.length && (
            <TableRow className="hover:bg-transparent">
              <TableCell colSpan={7} className="text-center">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setLimit((n) => n + 1000)}
                >
                  Show{" "}
                  {Math.min(
                    1000,
                    filtered.length - shown.length
                  ).toLocaleString()}{" "}
                  more
                </Button>
              </TableCell>
            </TableRow>
          )}
        </TableBody>
      </Table>
      {!filtered.length && (
        <Empty>
          <EmptyHeader>
            <EmptyTitle>
              {records.size ? "No statements match" : "No statements yet"}
            </EmptyTitle>
            <EmptyDescription>
              {records.size
                ? "Change the filters to see more."
                : "Statements from every client (drivers and worksheets alike) appear here as they run."}
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      )}
    </div>
  )

  return (
    <div className="flex h-full flex-col">
      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b px-3 py-2">
        <InputGroup className="h-8 w-72">
          <InputGroupAddon>
            <SearchIcon />
          </InputGroupAddon>
          <InputGroupInput
            type="search"
            placeholder="Search SQL, errors, query IDs, tags"
            aria-label="Search the history"
            value={text}
            onChange={(e) => setText(e.target.value)}
          />
        </InputGroup>
        <Select
          items={STATUSES}
          value={status}
          onValueChange={(v) => setParam("status", v ?? "")}
        >
          <SelectTrigger size="sm" aria-label="Status">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {STATUSES.map((s) => (
              <SelectItem key={s.value} value={s.value}>
                {s.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select
          items={sessionItems}
          value={session}
          onValueChange={(v) => setParam("session", v ?? "")}
        >
          <SelectTrigger size="sm" className="max-w-72" aria-label="Session">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {sessionItems.map((s) => (
              <SelectItem key={s.value} value={s.value}>
                {s.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Label className="font-normal">
          <Checkbox
            checked={hideUi}
            onCheckedChange={(checked) =>
              setParam("hideUi", checked ? "1" : "")
            }
          />
          Hide web UI queries
        </Label>
        <span className="text-xs text-muted-foreground">
          {plural(filtered.length, "statement")}
          {filtered.length !== records.size &&
            ` of ${records.size.toLocaleString()}`}
        </span>
        <div className="flex-1" />
        <Button
          variant="outline"
          size="sm"
          onClick={() => setPaused(paused ? null : live)}
        >
          {paused ? (
            <PlayIcon data-icon="inline-start" />
          ) : (
            <PauseIcon data-icon="inline-start" />
          )}
          {paused ? "Resume live updates" : "Pause"}
        </Button>
        <Button variant="outline" size="sm" onClick={() => void clear()}>
          <Trash2Icon data-icon="inline-start" />
          Clear
        </Button>
      </div>
      <div className="min-h-0 flex-1">
        {outlet ? (
          <ResizablePanelGroup
            orientation="horizontal"
            defaultLayout={layout.defaultLayout}
            onLayoutChanged={layout.onLayoutChanged}
          >
            <ResizablePanel id="list" minSize="30%">
              {list}
            </ResizablePanel>
            <ResizableHandle />
            <ResizablePanel id="detail" defaultSize="45%" minSize="25%">
              {outlet}
            </ResizablePanel>
          </ResizablePanelGroup>
        ) : (
          list
        )}
      </div>
    </div>
  )
}

type DetailData =
  | { record: HistoryRecord; missing?: undefined }
  | { missing: string; record?: undefined }

export async function historyRecordLoader({
  params,
}: LoaderFunctionArgs): Promise<DetailData> {
  try {
    const record = await api<HistoryRecord>(
      `/history/${encodeURIComponent(params.queryId ?? "")}`
    )
    return { record }
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) return { missing: e.message }
    throw e
  }
}

/** One statement: its facts, SQL, the DuckDB SQL it became, and its result or error. */
export function HistoryDetail() {
  const data = useLoaderData<DetailData>()
  const { queryId = "" } = useParams()
  const { search } = useLocation()
  const navigate = useNavigate()
  const { tab, setTab } = useOutletContext<DetailContext>()
  const revalidator = useRevalidator()
  const liveSequence = useStore(
    history,
    (s) => s.records.get(queryId)?.sequence
  )
  const record = data.record

  // Reload when the statement finishes (or changes).
  React.useEffect(() => {
    if (
      record &&
      liveSequence != null &&
      liveSequence !== record.sequence &&
      revalidator.state === "idle"
    ) {
      void revalidator.revalidate()
    }
  }, [liveSequence, record, revalidator])

  const closeLink = (
    <Link
      to={{ pathname: "/history", search }}
      aria-label="Close"
      className={buttonVariants({ variant: "ghost", size: "icon-sm" })}
    >
      <XIcon />
    </Link>
  )

  if (!record) {
    return (
      <div className="flex h-full flex-col">
        <div className="flex justify-end p-2">{closeLink}</div>
        <Empty>
          <EmptyHeader>
            <EmptyTitle>Not in the history</EmptyTitle>
            <EmptyDescription>{data.missing}</EmptyDescription>
          </EmptyHeader>
        </Empty>
      </div>
    )
  }

  const tabs: [string, string][] = [
    ["sql", "SQL"],
    ["duckdb", `DuckDB SQL (${record.duckdbSql?.length ?? 0})`],
  ]
  if (record.status === "succeeded") tabs.push(["result", "Result"])
  if (record.error) tabs.push(["error", "Error"])
  const active = tabs.some(([key]) => key === tab) ? tab : "sql"
  const duckdbSql = record.duckdbSql ?? []

  async function cancelQuery() {
    try {
      const result = await api<{ cancelled: boolean }>(
        `/queries/${encodeURIComponent(queryId)}/cancel`,
        { method: "POST" }
      )
      toast(result.cancelled ? "Cancelling…" : "It already finished")
    } catch (e) {
      toast.error(message(e))
    }
  }

  function openInWorksheet() {
    if (!record) return
    createWorksheet({
      name: `Query ${record.queryId.slice(0, 8)}`,
      sql: `${record.sql}${record.sql.trim().endsWith(";") ? "\n" : ";\n"}`,
      database: record.database,
      schema: record.schema,
    })
    void navigate("/")
  }

  const facts: [string, React.ReactNode][] = [
    ["Started", new Date(record.startedAt).toLocaleString()],
    [
      "Duration",
      record.status === "running"
        ? "running…"
        : formatDuration(record.durationMicros),
    ],
    ["Session", `#${record.sessionId} ${record.client}`],
    ["User", record.user],
    [
      "Context",
      [record.database, record.schema].filter(Boolean).join(".") || "none",
    ],
    ["Rows", record.rows ?? "—"],
    ["Kind", record.kind ?? "—"],
    ["Query tag", record.queryTag ?? "—"],
  ]

  return (
    <div className="flex h-full flex-col">
      <div className="shrink-0 space-y-3 border-b p-3">
        <div className="flex items-center gap-2">
          <StatusBadge status={record.status} />
          <span className="truncate font-mono text-xs">{record.queryId}</span>
          <Button
            variant="ghost"
            size="icon-xs"
            aria-label="Copy query ID"
            onClick={() => void copy(record.queryId, "Query ID copied")}
          >
            <CopyIcon />
          </Button>
          <div className="flex-1" />
          {record.status === "running" && (
            <Button
              variant="destructive"
              size="sm"
              onClick={() => void cancelQuery()}
            >
              <SquareIcon data-icon="inline-start" />
              Cancel
            </Button>
          )}
          <Button
            variant="outline"
            size="sm"
            title="Open this SQL in a new worksheet"
            onClick={openInWorksheet}
          >
            <ExternalLinkIcon data-icon="inline-start" />
            Open in worksheet
          </Button>
          {closeLink}
        </div>
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
          {facts.map(([term, value]) => (
            <div key={term} className="contents">
              <dt className="text-muted-foreground">{term}</dt>
              <dd className="truncate">{value}</dd>
            </div>
          ))}
        </dl>
      </div>
      <Tabs
        value={active}
        onValueChange={(value: string) => setTab(value)}
        className="min-h-0 flex-1 gap-0"
      >
        <TabsList variant="line" className="h-9 shrink-0 border-b px-2">
          {tabs.map(([key, label]) => (
            <TabsTrigger key={key} value={key} className="flex-none text-xs">
              {label}
            </TabsTrigger>
          ))}
        </TabsList>
        <div
          className={cn(
            "min-h-0 flex-1",
            active === "result"
              ? "flex flex-col"
              : "space-y-3 overflow-auto p-3"
          )}
        >
          {active === "sql" && <SqlCode sql={record.sql} />}
          {active === "duckdb" &&
            (duckdbSql.length ? (
              <>
                <p className="text-xs text-muted-foreground">
                  The DuckDB SQL localflake translated this statement into, in
                  the order it ran. Scripts and procedures run several.
                </p>
                {duckdbSql.map((sql, i) => (
                  <SqlCode
                    key={i}
                    sql={sql}
                    label={
                      duckdbSql.length > 1
                        ? `${i + 1} of ${duckdbSql.length}`
                        : null
                    }
                  />
                ))}
              </>
            ) : (
              <p className="text-xs text-muted-foreground">
                This statement didn't run any translated SQL (it was handled by
                localflake directly, or failed before reaching DuckDB).
              </p>
            ))}
          {active === "error" && record.error && (
            <QueryErrorView error={{ ...record.error, queryId: null }} />
          )}
          {active === "result" &&
            (record.resultAvailable ? (
              <StoredResult key={record.queryId} queryId={record.queryId} />
            ) : (
              <p className="p-3 text-xs text-muted-foreground">
                This result is no longer stored: localflake keeps the most
                recent few hundred results (the same ones RESULT_SCAN can read).
              </p>
            ))}
        </div>
      </Tabs>
    </div>
  )
}

/** A result localflake still keeps. */
function StoredResult({ queryId }: { queryId: string }) {
  const [state, setState] = React.useState<
    { result: Result } | { error: QueryError } | null
  >(null)
  React.useEffect(() => {
    const controller = new AbortController()
    api<Result & { error?: QueryError }>(
      `/results/${encodeURIComponent(queryId)}`,
      { signal: controller.signal }
    ).then(
      (data) => setState(data.error ? { error: data.error } : { result: data }),
      (e: unknown) => {
        if (!controller.signal.aborted)
          setState({ error: { message: message(e) } })
      }
    )
    return () => controller.abort()
  }, [queryId])
  if (!state) {
    return (
      <div className="flex justify-center p-6">
        <Spinner className="text-muted-foreground" />
      </div>
    )
  }
  if ("error" in state) {
    return (
      <div className="p-3">
        <QueryErrorView error={state.error} />
      </div>
    )
  }
  return <ResultPanel result={state.result} compact />
}
