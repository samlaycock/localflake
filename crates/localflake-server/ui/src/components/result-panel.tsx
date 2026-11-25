import {
  CheckIcon,
  CopyIcon,
  DownloadIcon,
  HashIcon,
  SearchIcon,
} from "lucide-react"
import * as React from "react"
import { toast } from "sonner"

import { MenuItems } from "@/components/menu-items"
import {
  ResultGrid,
  copy,
  type Cell,
  type Sort,
} from "@/components/result-grid"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import {
  DropdownMenu,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import {
  InputGroup,
  InputGroupAddon,
  InputGroupInput,
} from "@/components/ui/input-group"
import { api, message } from "@/lib/api"
import { downloadStageFile } from "@/lib/explorer"
import {
  compareValues,
  delimited,
  download,
  formatDuration,
  plural,
  rowObject,
  rowText,
  typeLabel,
} from "@/lib/format"
import type { Column, QueryError, Result, Row } from "@/lib/types"

/** Formatted text by row, computed as rows show. */
const texts = new WeakMap<Row, (string | null)[]>()
/** Rows loaded beyond the first page, kept while the result is. */
const loaded = new WeakMap<Result, Row[]>()

function textFor(result: Result, row: Row) {
  let text = texts.get(row)
  if (!text) {
    text = rowText(result.columns, row, result.timezone)
    texts.set(row, text)
  }
  return text
}

/** The rows to show, as indexes: those matching the filter, in the sort's order. */
function viewOf(result: Result, rows: Row[], filter: string, sort: Sort) {
  let indexes = rows.map((_, i) => i)
  const needle = filter.trim().toLowerCase()
  if (needle) {
    indexes = indexes.filter((i) =>
      textFor(result, rows[i]).some((t) => t?.toLowerCase().includes(needle))
    )
  }
  if (sort) {
    const column = result.columns[sort.column]
    indexes.sort(
      (a, b) =>
        sort.direction *
          compareValues(rows[a][sort.column], rows[b][sort.column], column) ||
        a - b
    )
  }
  return indexes
}

/** A result's grid with its toolbar: counts, timing, filter, copy and download. */
export function ResultPanel({
  result,
  compact = false,
}: {
  result: Result
  /** For the query history: no query ID or statement in the toolbar. */
  compact?: boolean
}) {
  const [rows, setRows] = React.useState(
    () => loaded.get(result) ?? result.rows
  )
  const [filter, setFilter] = React.useState("")
  const [sort, setSort] = React.useState<Sort>(null)
  const [loading, setLoading] = React.useState(false)
  const [open, setOpen] = React.useState<Cell | null>(null)
  const inFlight = React.useRef<Promise<Row[]> | null>(null)

  const textOf = React.useCallback(
    (index: number) => textFor(result, rows[index]),
    [rows, result]
  )
  const view = React.useMemo(
    () => viewOf(result, rows, filter, sort),
    [rows, filter, sort, result]
  )

  /** Loads the next page, or every row. */
  const loadMore = React.useCallback(
    (all: boolean) => {
      if (inFlight.current) return inFlight.current
      const promise = (async () => {
        let next = loaded.get(result) ?? result.rows
        setLoading(true)
        try {
          while (next.length < result.total) {
            const limit = all ? 100000 : 5000
            const data = await api<Result & { error?: QueryError }>(
              `/results/${encodeURIComponent(result.queryId)}?offset=${next.length}&limit=${limit}`
            )
            if (data.error) throw new Error(data.error.message)
            if (!data.rows.length) break
            next = [...next, ...data.rows]
            loaded.set(result, next)
            setRows(next)
            if (!all) break
          }
        } catch (e) {
          toast.error(message(e))
        } finally {
          setLoading(false)
          inFlight.current = null
        }
        return next
      })()
      inFlight.current = promise
      return promise
    },
    [result]
  )

  async function onSort(column: number) {
    if (rows.length < result.total) {
      if (result.total <= 200000) await loadMore(true)
      else {
        toast(`Sorting the ${rows.length.toLocaleString()} rows loaded so far`)
      }
    }
    setSort((current) =>
      current?.column !== column
        ? { column, direction: 1 }
        : current.direction === 1
          ? { column, direction: -1 }
          : null
    )
  }

  /** Every row the grid shows (once they're all loaded), in its order, as text. */
  async function allText() {
    const all = rows.length < result.total ? await loadMore(true) : rows
    return viewOf(result, all, filter, sort).map((i) => textFor(result, all[i]))
  }

  const name =
    /from\s+([\w."$]+)/i
      .exec(result.statement)?.[1]
      ?.replace(/["$]/g, "")
      .toLowerCase() ?? "result"
  const asJson = (text: (string | null)[][]) =>
    JSON.stringify(
      text.map((t) => rowObject(result.columns, t)),
      null,
      2
    )

  const listing = /^\s*(?:LIST|LS)\s+(@\S+)/i.exec(result.statement)
  const nameColumn = result.columns.findIndex((c) => c.name === "name")
  const footer = filter.trim()
    ? `${plural(view.length, "row")} match the filter${rows.length < result.total ? ` (of ${rows.length.toLocaleString()} loaded)` : ""}`
    : rows.length < result.total
      ? `Showing ${rows.length.toLocaleString()} of ${result.total.toLocaleString()} rows. Scroll to load more.`
      : ""

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-3 border-b px-3 py-1.5 text-xs">
        <span className="flex items-center gap-1.5 whitespace-nowrap">
          <CheckIcon className="size-3.5 text-primary" />
          <span className="font-medium">{plural(result.total, "row")}</span>
          {rows.length < result.total && (
            <span className="text-muted-foreground">
              ({rows.length.toLocaleString()} loaded)
            </span>
          )}
        </span>
        {result.durationMicros != null && (
          <span
            className="whitespace-nowrap text-muted-foreground"
            title="Execution time"
          >
            {formatDuration(result.durationMicros)}
          </span>
        )}
        {!compact && (
          <Button
            variant="ghost"
            size="xs"
            className="font-mono font-normal text-muted-foreground"
            title="Query ID: click to copy"
            onClick={() => void copy(result.queryId, "Query ID copied")}
          >
            <HashIcon data-icon="inline-start" />
            {result.queryId.slice(0, 13)}…
          </Button>
        )}
        {!compact && (
          <span
            className="min-w-0 flex-1 truncate font-mono text-muted-foreground"
            title={result.statement}
          >
            {result.statement}
          </span>
        )}
        {compact && <span className="flex-1" />}
        <InputGroup className="h-7 w-48 shrink-0">
          <InputGroupAddon>
            <SearchIcon />
          </InputGroupAddon>
          <InputGroupInput
            type="search"
            placeholder="Filter rows"
            aria-label="Filter rows"
            className="text-xs"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
        </InputGroup>
        <DropdownMenu>
          <DropdownMenuTrigger
            render={<Button variant="outline" size="sm" disabled={loading} />}
          >
            <DownloadIcon data-icon="inline-start" />
            Export
          </DropdownMenuTrigger>
          <MenuItems
            as="dropdown"
            items={[
              {
                label: "Copy as TSV (for spreadsheets)",
                icon: CopyIcon,
                action: async () =>
                  copy(
                    delimited(result.columns, await allText(), "\t"),
                    "Rows copied"
                  ),
              },
              {
                label: "Copy as JSON",
                icon: CopyIcon,
                action: async () =>
                  copy(asJson(await allText()), "Rows copied"),
              },
              null,
              {
                label: "Download CSV",
                icon: DownloadIcon,
                action: async () =>
                  download(
                    `${name}.csv`,
                    delimited(result.columns, await allText(), ","),
                    "text/csv"
                  ),
              },
              {
                label: "Download JSON",
                icon: DownloadIcon,
                action: async () =>
                  download(
                    `${name}.json`,
                    asJson(await allText()),
                    "application/json"
                  ),
              },
            ]}
          />
        </DropdownMenu>
      </div>
      <ResultGrid
        columns={result.columns}
        rows={rows}
        view={view}
        textOf={textOf}
        total={result.total}
        sort={sort}
        onSort={(c) => void onSort(c)}
        onOpen={setOpen}
        onFilterRequest={setFilter}
        onNearEnd={() => {
          if (!filter.trim() && !sort && rows.length < result.total) {
            void loadMore(false)
          }
        }}
        extraActions={
          listing && nameColumn >= 0
            ? (_cell, text) => [
                null,
                {
                  label: "Download file",
                  icon: DownloadIcon,
                  action: () =>
                    downloadStageFile(listing[1], text[nameColumn] ?? ""),
                },
              ]
            : undefined
        }
        footer={footer}
      />
      <ValueDialog
        column={open ? result.columns[open.column] : null}
        raw={open ? rows[open.row][open.column] : null}
        text={open ? textOf(open.row)[open.column] : null}
        onClose={() => setOpen(null)}
      />
    </div>
  )
}

/** All of one value: JSON pretty-printed. */
function ValueDialog({
  column,
  raw,
  text,
  onClose,
}: {
  column: Column | null
  raw: string | null
  text: string | null
  onClose: () => void
}) {
  let shown = text ?? "NULL"
  if (column && raw != null) {
    if (["variant", "object", "array"].includes(column.type)) shown = raw
    else if (column.type === "text") {
      try {
        const parsed: unknown = JSON.parse(raw)
        if (parsed && typeof parsed === "object") {
          shown = JSON.stringify(parsed, null, 2)
        }
      } catch {
        // Not JSON.
      }
    }
  }
  return (
    <Dialog open={column != null} onOpenChange={(next) => !next && onClose()}>
      <DialogContent className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <span className="truncate font-mono">{column?.name}</span>
            {column && (
              <Badge variant="secondary" className="font-mono">
                {typeLabel(column)}
              </Badge>
            )}
          </DialogTitle>
        </DialogHeader>
        <pre
          className={
            raw == null
              ? "font-mono text-sm text-muted-foreground italic"
              : "max-h-[60vh] overflow-auto rounded-lg border bg-muted/30 p-3 font-mono text-xs leading-5 break-words whitespace-pre-wrap"
          }
        >
          {shown}
        </pre>
        <DialogFooter className="items-center sm:justify-between">
          <span className="text-xs text-muted-foreground">
            {raw == null ? "" : plural(shown.length, "character")}
          </span>
          <div className="flex gap-2">
            <Button
              variant="outline"
              onClick={() => void copy(text ?? "", "Value copied")}
            >
              <CopyIcon data-icon="inline-start" />
              Copy
            </Button>
            <Button onClick={onClose}>Close</Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
