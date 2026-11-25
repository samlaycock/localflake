import {
  CircleXIcon,
  DatabaseIcon,
  ListVideoIcon,
  PlayIcon,
  PlusIcon,
  SaveIcon,
  SquareIcon,
  XIcon,
} from "lucide-react"
import * as React from "react"
import { useDefaultLayout } from "react-resizable-panels"

import { Explorer } from "@/components/explorer"
import { QueryErrorView } from "@/components/query-error"
import { ResultPanel } from "@/components/result-panel"
import { SavedQueries } from "@/components/saved-queries"
import { SqlEditor, type EditorCommand } from "@/components/sql-editor"
import { Alert, AlertDescription } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@/components/ui/empty"
import { Input } from "@/components/ui/input"
import { Kbd, KbdGroup } from "@/components/ui/kbd"
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
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import { catalog, loadDatabase } from "@/lib/catalog"
import { completions } from "@/lib/completions"
import { MOD, debounce } from "@/lib/format"
import { saved } from "@/lib/saved"
import { layoutStorage, load, save } from "@/lib/storage"
import { useStore } from "@/lib/store"
import type { QueryError, Result } from "@/lib/types"
import { cn } from "@/lib/utils"
import {
  activate,
  cancel,
  closeWorksheet,
  createWorksheet,
  editors,
  getWorksheet,
  isDirty,
  renameWorksheet,
  run,
  saveWorksheet,
  setActiveResult,
  setSql,
  statementAt,
  statementRanges,
  switchContext,
  worksheets,
  type EditorHandle,
  type Running,
  type Worksheet,
} from "@/lib/worksheets"

/** The explorer and saved queries beside the worksheets. */
export function WorksheetsView({ className }: { className?: string }) {
  const layout = useDefaultLayout({
    id: "worksheets-layout",
    storage: layoutStorage,
  })
  return (
    <div className={cn("h-full", className)}>
      <ResizablePanelGroup
        orientation="horizontal"
        defaultLayout={layout.defaultLayout}
        onLayoutChanged={layout.onLayoutChanged}
      >
        <ResizablePanel
          id="sidebar"
          defaultSize="280px"
          minSize="180px"
          maxSize="600px"
        >
          <Sidebar />
        </ResizablePanel>
        <ResizableHandle />
        <ResizablePanel id="workspace" minSize="40%">
          <Workspace />
        </ResizablePanel>
      </ResizablePanelGroup>
    </div>
  )
}

function Sidebar() {
  const [tab, setTab] = React.useState(() => load("sideTab", "explorer"))
  const count = useStore(saved, (items) => items.length)
  return (
    <Tabs
      value={tab}
      onValueChange={(value: string) => {
        setTab(value)
        save("sideTab", value)
      }}
      className="h-full gap-0"
    >
      <TabsList variant="line" className="h-10 w-full shrink-0 border-b px-2">
        <TabsTrigger value="explorer">Explorer</TabsTrigger>
        <TabsTrigger value="saved">
          Saved queries
          <Badge variant="secondary" className="tabular-nums">
            {count}
          </Badge>
        </TabsTrigger>
      </TabsList>
      <TabsContent value="explorer" className="flex min-h-0 flex-1 flex-col">
        <Explorer />
      </TabsContent>
      <TabsContent value="saved" className="flex min-h-0 flex-1 flex-col">
        <SavedQueries />
      </TabsContent>
    </Tabs>
  )
}

function Workspace() {
  const list = useStore(worksheets, (s) => s.list)
  const active = useStore(worksheets, (s) => s.activeId)
  return (
    <div className="flex h-full flex-col">
      <WorksheetTabs />
      {list.map((ws) => (
        <WorksheetPane
          key={ws.model.id}
          id={ws.model.id}
          active={ws.model.id === active}
        />
      ))}
    </div>
  )
}

function WorksheetTabs() {
  const list = useStore(worksheets, (s) => s.list)
  const active = useStore(worksheets, (s) => s.activeId)
  return (
    <div
      role="tablist"
      aria-label="Worksheets"
      className="flex h-10 shrink-0 items-end gap-0.5 overflow-x-auto border-b px-2"
    >
      {list.map((ws) => (
        <WorksheetTab
          key={ws.model.id}
          ws={ws}
          active={ws.model.id === active}
        />
      ))}
      <Tooltip>
        <TooltipTrigger
          render={
            <Button
              variant="ghost"
              size="icon-sm"
              className="mb-1.5"
              aria-label="New worksheet"
              onClick={() => createWorksheet()}
            />
          }
        >
          <PlusIcon />
        </TooltipTrigger>
        <TooltipContent>New worksheet</TooltipContent>
      </Tooltip>
    </div>
  )
}

function WorksheetTab({ ws, active }: { ws: Worksheet; active: boolean }) {
  const [renaming, setRenaming] = React.useState(false)
  const { id, name, savedId } = ws.model
  return (
    <div
      className={cn(
        "group/tab relative flex h-9 max-w-56 shrink-0 items-center gap-1 rounded-t-md pr-1 pl-2.5 text-sm text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground",
        active &&
          "text-foreground after:absolute after:inset-x-0 after:bottom-0 after:h-0.5 after:bg-primary"
      )}
      onAuxClick={(e) => {
        if (e.button === 1) void closeWorksheet(id)
      }}
    >
      {ws.running ? (
        <Spinner className="size-3.5 text-primary" />
      ) : savedId ? (
        <SaveIcon className="size-3.5 shrink-0" />
      ) : null}
      {renaming ? (
        <Input
          autoFocus
          defaultValue={name}
          aria-label="Worksheet name"
          className="h-6 w-40 px-1.5 text-sm"
          onFocus={(e) => e.target.select()}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur()
            if (e.key === "Escape") setRenaming(false)
          }}
          onBlur={(e) => {
            if (renaming) renameWorksheet(id, e.target.value)
            setRenaming(false)
          }}
        />
      ) : (
        <button
          type="button"
          role="tab"
          aria-selected={active}
          title={savedId ? `${name} (saved query)` : name}
          className="truncate py-1 outline-none focus-visible:underline"
          onClick={() => !active && activate(id)}
          onDoubleClick={() => setRenaming(true)}
        >
          {name}
        </button>
      )}
      {isDirty(ws) && (
        <span
          title="Changed since saved"
          className="size-1.5 shrink-0 rounded-full bg-primary"
        />
      )}
      <Button
        variant="ghost"
        size="icon-xs"
        aria-label={`Close ${name}`}
        className="opacity-60 group-hover/tab:opacity-100"
        onClick={() => void closeWorksheet(id)}
      >
        <XIcon />
      </Button>
    </div>
  )
}

type Range = [number, number]

function WorksheetPane({ id, active }: { id: string; active: boolean }) {
  const ws = useStore(worksheets, (s) => s.list.find((w) => w.model.id === id))
  const [statement, setStatement] = React.useState<Range | null>(null)
  const layout = useDefaultLayout({
    id: "worksheet-layout",
    storage: layoutStorage,
  })
  const database = ws?.model.database ?? null

  const editorRef = React.useCallback(
    (handle: EditorHandle | null) => {
      if (handle) editors.set(id, handle)
      else editors.delete(id)
    },
    [id]
  )

  // Mark the statement the cursor is in, when there's more than one.
  const updateMark = React.useMemo(
    () =>
      debounce(async () => {
        const sql = getWorksheet(id)?.model.sql ?? ""
        const editor = editors.get(id)
        if (!sql.trim() || !editor?.focused()) {
          setStatement(null)
          return
        }
        try {
          const ranges = await statementRanges(id, sql)
          setStatement(
            ranges.length > 1
              ? statementAt(ranges, editor.selection().start)
              : null
          )
        } catch {
          setStatement(null)
        }
      }, 120),
    [id]
  )

  // The context pickers list the database's schemas.
  React.useEffect(() => {
    if (database) loadDatabase(database).catch(() => undefined)
  }, [database])

  const onCommand = React.useCallback(
    (command: EditorCommand) => {
      if (command === "save") void saveWorksheet(id)
      else void run(id, command === "runAll" ? "all" : "cursor")
    },
    [id]
  )

  const getCompletions = React.useCallback(
    (word: Parameters<typeof completions>[0], text: string, offset: number) => {
      const current = getWorksheet(id)?.model
      return completions(
        word,
        text,
        offset,
        current?.database ?? null,
        current?.schema ?? null
      )
    },
    [id]
  )

  const onChange = React.useCallback(
    (sql: string) => {
      setSql(id, sql)
      updateMark()
    },
    [id, updateMark]
  )

  if (!ws) return null
  const running = ws.running != null
  return (
    <div className={cn("min-h-0 flex-1 flex-col", active ? "flex" : "hidden")}>
      <div className="flex shrink-0 items-center gap-2 border-b px-3 py-1.5">
        <ContextPicker ws={ws} />
        <div className="flex-1" />
        <Tooltip>
          <TooltipTrigger
            render={
              <Button
                variant="ghost"
                size="sm"
                onClick={() => void saveWorksheet(id)}
              />
            }
          >
            <SaveIcon data-icon="inline-start" />
            Save
          </TooltipTrigger>
          <TooltipContent>Save as a saved query ({MOD}+S)</TooltipContent>
        </Tooltip>
        {running && (
          <Button
            variant="destructive"
            size="sm"
            onClick={() => void cancel(id)}
          >
            <SquareIcon data-icon="inline-start" />
            Cancel
          </Button>
        )}
        <Tooltip>
          <TooltipTrigger
            render={
              <Button
                variant="outline"
                size="sm"
                disabled={running}
                onClick={() => void run(id, "all")}
              />
            }
          >
            <ListVideoIcon data-icon="inline-start" />
            Run all
          </TooltipTrigger>
          <TooltipContent>
            Run every statement ({MOD}+Shift+Enter)
          </TooltipContent>
        </Tooltip>
        <Tooltip>
          <TooltipTrigger
            render={
              <Button
                size="sm"
                disabled={running}
                onClick={() => void run(id, "cursor")}
              />
            }
          >
            <PlayIcon data-icon="inline-start" />
            Run
            <KbdGroup className="ml-1">
              <Kbd className="bg-primary-foreground/15 text-primary-foreground">
                {MOD}
              </Kbd>
              <Kbd className="bg-primary-foreground/15 text-primary-foreground">
                ↵
              </Kbd>
            </KbdGroup>
          </TooltipTrigger>
          <TooltipContent>
            Run the statement at the cursor, or the selection
          </TooltipContent>
        </Tooltip>
      </div>
      <ResizablePanelGroup
        orientation="vertical"
        className="min-h-0 flex-1"
        defaultLayout={layout.defaultLayout}
        onLayoutChanged={layout.onLayoutChanged}
      >
        <ResizablePanel id="editor" defaultSize="42%" minSize="12%">
          <SqlEditor
            ref={editorRef}
            className="h-full"
            value={ws.model.sql}
            onChange={onChange}
            onCommand={onCommand}
            onCaret={updateMark}
            statement={statement}
            getCompletions={getCompletions}
            placeholder={`Write SQL here.  ${MOD}+Enter runs the statement at the cursor, ${MOD}+Shift+Enter runs everything.`}
          />
        </ResizablePanel>
        <ResizableHandle />
        <ResizablePanel id="results" minSize="15%">
          <Results ws={ws} />
        </ResizablePanel>
      </ResizablePanelGroup>
    </div>
  )
}

/** The worksheet's database and schema. */
function ContextPicker({ ws }: { ws: Worksheet }) {
  const { id, database, schema } = ws.model
  const databases = useStore(catalog, (s) => s.databases)
  const loaded = useStore(catalog, (s) =>
    database ? s.byDatabase[database] : undefined
  )
  const databaseItems = [
    { label: "No database", value: "" },
    ...[
      ...new Set([
        ...(databases ?? []).map((d) => d.name),
        ...(database ? [database] : []),
      ]),
    ].map((name) => ({ label: name, value: name })),
  ]
  const schemaItems = [
    { label: "No schema", value: "" },
    ...[
      ...new Set([
        ...(loaded?.schemas ?? []).map((s) => s.name),
        ...(schema ? [schema] : []),
      ]),
    ].map((name) => ({ label: name, value: name })),
  ]
  return (
    <div className="flex min-w-0 items-center gap-1.5">
      <DatabaseIcon className="size-4 shrink-0 text-muted-foreground" />
      <Select
        items={databaseItems}
        value={database ?? ""}
        onValueChange={(value) => void switchContext(id, value || null, null)}
      >
        <SelectTrigger
          size="sm"
          className="max-w-48 font-mono text-xs"
          aria-label="Database"
        >
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {databaseItems.map((item) => (
            <SelectItem
              key={item.value}
              value={item.value}
              className="font-mono text-xs"
            >
              {item.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <span className="text-muted-foreground">.</span>
      <Select
        items={schemaItems}
        value={schema ?? ""}
        disabled={!database}
        onValueChange={(value) =>
          void switchContext(id, database, value || null)
        }
      >
        <SelectTrigger
          size="sm"
          className="max-w-48 font-mono text-xs"
          aria-label="Schema"
        >
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {schemaItems.map((item) => (
            <SelectItem
              key={item.value}
              value={item.value}
              className="font-mono text-xs"
            >
              {item.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </div>
  )
}

type Entry = Result | { error: QueryError }

function resultSummary(result: Result) {
  if (result.kind === "query" || result.kind === "show") {
    return `${result.total.toLocaleString()} ${result.total === 1 ? "row" : "rows"}`
  }
  if (["insert", "update", "delete", "merge"].includes(result.kind)) {
    return `${result.kind} ${result.rows[0]?.[0] ?? ""}`.trim()
  }
  if (result.columns[0]?.name === "status") {
    const status = String(result.rows[0]?.[0] ?? result.kind)
    return status.length > 40 ? `${status.slice(0, 39)}…` : status
  }
  return `${result.total.toLocaleString()} ${result.total === 1 ? "row" : "rows"}`
}

function Results({ ws }: { ws: Worksheet }) {
  const { id } = ws.model
  if (ws.running) return <RunningView id={id} running={ws.running} />
  const entries: Entry[] = [...ws.results]
  if (ws.error) entries.push({ error: ws.error })
  if (!entries.length) {
    return (
      <Empty className="h-full">
        <EmptyHeader>
          <EmptyTitle>Results appear here</EmptyTitle>
          <EmptyDescription>
            <KbdGroup>
              <Kbd>{MOD}</Kbd>
              <Kbd>Enter</Kbd>
            </KbdGroup>{" "}
            runs the statement at the cursor (or the selection),{" "}
            <KbdGroup>
              <Kbd>{MOD}</Kbd>
              <Kbd>Shift</Kbd>
              <Kbd>Enter</Kbd>
            </KbdGroup>{" "}
            runs them all. Every statement any client runs is in the query
            history.
          </EmptyDescription>
        </EmptyHeader>
      </Empty>
    )
  }
  const index = Math.min(ws.activeResult, entries.length - 1)
  const entry = entries[index]
  return (
    <div className="flex h-full flex-col">
      {entries.length > 1 && (
        <Tabs
          value={String(index)}
          onValueChange={(value: string) => setActiveResult(id, Number(value))}
          className="shrink-0 gap-0 overflow-x-auto border-b px-2"
        >
          <TabsList variant="line" className="h-9">
            {entries.map((e, i) => (
              <TabsTrigger
                key={i}
                value={String(i)}
                title={"error" in e ? e.error.message : e.statement}
                className="flex-none text-xs"
              >
                {"error" in e ? (
                  <>
                    <CircleXIcon className="text-destructive" />
                    Error
                  </>
                ) : (
                  `${i + 1}. ${resultSummary(e)}`
                )}
              </TabsTrigger>
            ))}
          </TabsList>
        </Tabs>
      )}
      {"error" in entry ? (
        <div className="min-h-0 flex-1 overflow-auto p-3">
          <QueryErrorView error={entry.error} />
        </div>
      ) : entry.transfer ? (
        <div className="p-3">
          <Alert>
            <AlertDescription>
              PUT and GET move files between a driver and a stage, so they can't
              run in the browser. To upload a file, use “Upload file…” on a
              stage in the explorer, or “Load data…” on a table.
            </AlertDescription>
          </Alert>
        </div>
      ) : (
        <ResultPanel key={entry.queryId} result={entry} />
      )}
    </div>
  )
}

function RunningView({ id, running }: { id: string; running: Running }) {
  const [now, setNow] = React.useState(() => performance.now())
  React.useEffect(() => {
    const timer = setInterval(() => setNow(performance.now()), 100)
    return () => clearInterval(timer)
  }, [])
  return (
    <Empty className="h-full">
      <EmptyHeader>
        <Spinner className="size-6 text-primary" />
        <EmptyTitle className="max-w-xl truncate font-mono text-sm font-normal">
          {running.label ?? "Running…"}
        </EmptyTitle>
        <EmptyDescription className="tabular-nums">
          {(Math.max(0, now - running.started) / 1000).toFixed(1)} s
        </EmptyDescription>
      </EmptyHeader>
      <Button variant="destructive" size="sm" onClick={() => void cancel(id)}>
        <SquareIcon data-icon="inline-start" />
        Cancel
      </Button>
    </Empty>
  )
}
