import {
  ChevronRightIcon,
  Columns3Icon,
  CopyIcon,
  DatabaseIcon,
  EraserIcon,
  EyeIcon,
  FileCogIcon,
  FolderIcon,
  HashIcon,
  InfoIcon,
  ListIcon,
  ListOrderedIcon,
  MoreHorizontalIcon,
  PlusIcon,
  RefreshCwIcon,
  SearchIcon,
  SquareFunctionIcon,
  TableIcon,
  TargetIcon,
  TerminalIcon,
  TextCursorInputIcon,
  Trash2Icon,
  UploadIcon,
  WarehouseIcon,
  type LucideIcon,
} from "lucide-react"
import * as React from "react"
import { toast } from "sonner"

import { copy } from "@/components/result-grid"
import { LoadDataDialog, UploadDialog } from "@/components/stage-dialogs"
import { MenuItems, type MenuItem } from "@/components/menu-items"
import { Button } from "@/components/ui/button"
import { ContextMenu, ContextMenuTrigger } from "@/components/ui/context-menu"
import {
  DropdownMenu,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import {
  InputGroup,
  InputGroupAddon,
  InputGroupInput,
} from "@/components/ui/input-group"
import { ScrollArea } from "@/components/ui/scroll-area"
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import { message } from "@/lib/api"
import { catalog, loadDatabase, loadDatabases } from "@/lib/catalog"
import {
  createObject,
  dropObject,
  expanded,
  insertIntoEditor,
  preview,
  toggle,
} from "@/lib/explorer"
import { compactNumber, plural } from "@/lib/format"
import { ident, qualified } from "@/lib/sql"
import { useStore } from "@/lib/store"
import type { CatalogObject, Named, Routine, Schema } from "@/lib/types"
import { cn } from "@/lib/utils"
import { activeId, createWorksheet, switchContext } from "@/lib/worksheets"

type TreeNode = {
  key: string
  depth: number
  label: string
  icon?: LucideIcon
  open?: boolean
  expandable?: boolean
  meta?: string
  title?: string
  dim?: boolean
  group?: boolean
  onToggle?: () => void
  /** Double-click: insert the name into the worksheet. */
  onInsert?: () => void
  /** The name to preview with the eye button (tables and views). */
  preview?: string
  menu?: () => MenuItem[]
  menuTitle?: string
}

type TreeLine = TreeNode | { message: string; depth: number; error?: boolean }

type Dialog =
  | { kind: "upload"; stage: string }
  | { kind: "load"; database: string; schema: string; table: CatalogObject }
  | null

/** Databases, schemas and their objects, with actions on each. */
export function Explorer() {
  const state = useStore(catalog)
  const open = useStore(expanded)
  const [filter, setFilter] = React.useState("")
  const [target, setTarget] = React.useState<TreeNode | null>(null)
  const [dialog, setDialog] = React.useState<Dialog>(null)

  const reload = React.useCallback(async () => {
    try {
      await loadDatabases(true)
      await Promise.all(
        [...expanded.get()]
          .filter((k) => k.startsWith("db:"))
          .map((k) => loadDatabase(k.slice(3), true).catch(() => undefined))
      )
    } catch (e) {
      catalog.set((s) => ({ ...s, error: message(e) }))
    }
  }, [])

  React.useEffect(() => {
    void reload()
  }, [reload])

  // Expanded databases load as they're opened (or were open last time).
  React.useEffect(() => {
    for (const key of open) {
      const name = key.slice(3)
      if (
        key.startsWith("db:") &&
        state.databases?.some((d) => d.name === name) &&
        !state.byDatabase[name]
      ) {
        loadDatabase(name).catch((e: unknown) => toast.error(message(e)))
      }
    }
  }, [open, state])

  const lines = buildTree({
    state,
    open,
    filter: filter.trim().toUpperCase(),
    onUpload: (stage) => setDialog({ kind: "upload", stage }),
    onLoad: (database, schema, table) =>
      setDialog({ kind: "load", database, schema, table }),
  })

  return (
    <>
      <div className="flex shrink-0 items-center gap-1 p-2">
        <InputGroup className="h-8">
          <InputGroupAddon>
            <SearchIcon />
          </InputGroupAddon>
          <InputGroupInput
            type="search"
            placeholder="Filter objects"
            aria-label="Filter objects"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
        </InputGroup>
        <IconButton
          icon={RefreshCwIcon}
          label="Refresh"
          onClick={() => void reload()}
        />
        <IconButton
          icon={PlusIcon}
          label="Create database"
          onClick={() => void createObject("database")}
        />
      </div>
      <ScrollArea className="min-h-0 flex-1">
        <ContextMenu>
          <ContextMenuTrigger
            role="tree"
            aria-label="Objects"
            className="flex flex-col px-1.5 pb-2"
            onContextMenu={(e) => {
              const key = (e.target as HTMLElement).closest<HTMLElement>(
                "[data-key]"
              )?.dataset.key
              const node = lines.find(
                (l): l is TreeNode => "key" in l && l.key === key
              )
              setTarget(node?.menu ? node : null)
            }}
          >
            {lines.map((line, i) =>
              "key" in line ? (
                <TreeRow key={line.key} node={line} />
              ) : (
                <div
                  key={`message-${i}`}
                  className={cn(
                    "py-1.5 pr-2 text-xs text-muted-foreground",
                    line.error && "text-destructive"
                  )}
                  style={{ paddingLeft: 8 + line.depth * 14 }}
                >
                  {line.message}
                </div>
              )
            )}
          </ContextMenuTrigger>
          {target?.menu && (
            <MenuItems
              as="context"
              items={target.menu()}
              title={target.menuTitle ?? target.label}
            />
          )}
        </ContextMenu>
      </ScrollArea>
      {dialog?.kind === "upload" && (
        <UploadDialog stage={dialog.stage} onClose={() => setDialog(null)} />
      )}
      {dialog?.kind === "load" && (
        <LoadDataDialog
          database={dialog.database}
          schema={dialog.schema}
          table={dialog.table}
          onClose={() => setDialog(null)}
        />
      )}
    </>
  )
}

function IconButton({
  icon: Icon,
  label,
  onClick,
}: {
  icon: LucideIcon
  label: string
  onClick: () => void
}) {
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={label}
            onClick={onClick}
          />
        }
      >
        <Icon />
      </TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  )
}

function TreeRow({ node }: { node: TreeNode }) {
  const Icon = node.icon
  const toggleNode = node.onToggle ?? (() => toggle(node.key))
  return (
    <div
      role="treeitem"
      aria-level={node.depth + 1}
      aria-expanded={node.expandable ? !!node.open : undefined}
      aria-selected={false}
      tabIndex={-1}
      data-key={node.key}
      title={node.title ?? node.label}
      className={cn(
        "group/node relative flex h-7 shrink-0 cursor-default items-center gap-1.5 rounded-md pr-1.5 text-sm hover:bg-muted",
        node.group && "text-xs font-medium text-muted-foreground",
        node.dim && "text-muted-foreground"
      )}
      style={{ paddingLeft: 4 + node.depth * 14 }}
      onClick={() => node.expandable && toggleNode()}
      onDoubleClick={(e) => {
        if (!node.onInsert) return
        e.preventDefault()
        node.onInsert()
      }}
    >
      <span className="flex size-4 shrink-0 items-center justify-center">
        {node.expandable && (
          <ChevronRightIcon
            className={cn(
              "size-3.5 text-muted-foreground transition-transform",
              node.open && "rotate-90"
            )}
          />
        )}
      </span>
      {Icon && <Icon className="size-4 shrink-0 text-muted-foreground" />}
      <span className={cn("truncate", !node.group && "font-mono text-xs")}>
        {node.label}
      </span>
      {node.meta && (
        <span className="ml-auto shrink-0 truncate pl-2 font-mono text-[11px] text-muted-foreground">
          {node.meta}
        </span>
      )}
      {(node.preview || node.menu) && (
        <span className="absolute inset-y-0 right-1 hidden items-center gap-0.5 bg-muted pl-1 group-hover/node:flex focus-within:flex has-aria-expanded:flex">
          {node.preview && (
            <Button
              variant="ghost"
              size="icon-xs"
              aria-label="Preview data"
              title="Preview data"
              onClick={(e) => {
                e.stopPropagation()
                void preview(node.preview ?? "")
              }}
            >
              <EyeIcon />
            </Button>
          )}
          {node.menu && (
            <DropdownMenu>
              <DropdownMenuTrigger
                render={
                  <Button
                    variant="ghost"
                    size="icon-xs"
                    aria-label="Actions"
                    onClick={(e) => e.stopPropagation()}
                  />
                }
              >
                <MoreHorizontalIcon />
              </DropdownMenuTrigger>
              <MenuItems
                as="dropdown"
                items={node.menu()}
                title={node.menuTitle ?? node.label}
              />
            </DropdownMenu>
          )}
        </span>
      )}
    </div>
  )
}

// The tree --------------------------------------------------------------------------------------

type BuildOptions = {
  state: ReturnType<typeof catalog.get>
  open: ReadonlySet<string>
  filter: string
  onUpload: (stage: string) => void
  onLoad: (database: string, schema: string, table: CatalogObject) => void
}

function buildTree(options: BuildOptions): TreeLine[] {
  const { state, filter } = options
  const lines: TreeLine[] = []
  if (state.error) lines.push({ message: state.error, depth: 0, error: true })
  else if (!state.databases) lines.push({ message: "Loading…", depth: 0 })
  else if (!state.databases.length) {
    lines.push({
      message:
        "No databases yet. Create one with the + button, or connect a driver with a database name.",
      depth: 0,
    })
  }
  for (const db of state.databases ?? [])
    databaseLines(db.name, db.comment, options, lines)
  if (filter && state.databases && !lines.length) {
    lines.push({
      message:
        "Nothing matches. The filter searches databases you've expanded.",
      depth: 0,
    })
  }
  return lines
}

function databaseLines(
  database: string,
  comment: string | null,
  options: BuildOptions,
  lines: TreeLine[]
) {
  const { state, open, filter } = options
  const key = `db:${database}`
  const data = state.byDatabase[database]
  const isOpen = open.has(key) || (!!filter && !!data)
  const children: TreeLine[] = []
  if (isOpen) {
    if (!data) children.push({ message: "Loading…", depth: 2 })
    else {
      const schemas = [...data.schemas].sort(
        (a, b) =>
          Number(a.name === "INFORMATION_SCHEMA") -
            Number(b.name === "INFORMATION_SCHEMA") ||
          a.name.localeCompare(b.name)
      )
      for (const schema of schemas)
        schemaLines(database, schema, options, children)
    }
  }
  if (filter && !database.toUpperCase().includes(filter) && !children.length) {
    return
  }
  lines.push({
    key,
    depth: 0,
    label: database,
    icon: DatabaseIcon,
    open: isOpen,
    expandable: true,
    meta: comment ?? undefined,
    menu: () => databaseMenu(database),
  })
  lines.push(...children)
}

function databaseMenu(database: string): MenuItem[] {
  return [
    {
      label: "Use in worksheet",
      icon: TargetIcon,
      action: () => switchContext(activeId(), database, null),
    },
    {
      label: "New worksheet here",
      icon: PlusIcon,
      action: () => createWorksheet({ database, schema: null }),
    },
    {
      label: "Create schema…",
      icon: FolderIcon,
      action: () => createObject("schema", database),
    },
    {
      label: "Refresh",
      icon: RefreshCwIcon,
      action: () =>
        loadDatabase(database, true).catch((e: unknown) =>
          toast.error(message(e))
        ),
    },
    {
      label: "Copy name",
      icon: CopyIcon,
      action: () => copy(ident(database), "Name copied"),
    },
    null,
    {
      label: "Drop database…",
      icon: Trash2Icon,
      danger: true,
      action: () =>
        dropObject(`DROP DATABASE ${ident(database)}`, `database ${database}`),
    },
  ]
}

const GROUPS: [
  string,
  (s: Schema) => (CatalogObject | Routine | Named)[],
  Kind,
][] = [
  ["Tables", (s) => s.objects.filter((o) => !o.kind.includes("VIEW")), "table"],
  ["Views", (s) => s.objects.filter((o) => o.kind.includes("VIEW")), "view"],
  ["Procedures", (s) => s.procedures, "proc"],
  ["Functions", (s) => s.functions, "func"],
  ["Stages", (s) => s.stages, "stage"],
  ["Sequences", (s) => s.sequences, "seq"],
  ["File formats", (s) => s.fileFormats, "format"],
]

type Kind = "table" | "view" | "proc" | "func" | "stage" | "seq" | "format"

function schemaLines(
  database: string,
  schema: Schema,
  options: BuildOptions,
  lines: TreeLine[]
) {
  const { open, filter } = options
  const key = `schema:${database}.${schema.name}`
  const isOpen = open.has(key) || !!filter
  const children: TreeLine[] = []
  if (isOpen) {
    for (const [title, itemsOf, kind] of GROUPS) {
      const items = itemsOf(schema)
      const matching = filter
        ? items.filter(
            (i) =>
              i.name.toUpperCase().includes(filter) ||
              ("columns" in i &&
                i.columns.some((c) => c.name.toUpperCase().includes(filter)))
          )
        : items
      if (!matching.length) continue
      const groupKey = `group:${database}.${schema.name}:${title}`
      const groupOpen = !open.has(`${groupKey}:closed`) || !!filter
      children.push({
        key: groupKey,
        depth: 2,
        label: title,
        meta: String(matching.length),
        group: true,
        open: groupOpen,
        expandable: true,
        onToggle: () => toggle(`${groupKey}:closed`),
      })
      if (groupOpen) {
        for (const item of matching) {
          itemLines(database, schema.name, item, kind, options, children)
        }
      }
    }
    if (!children.length && !filter)
      children.push({ message: "Empty", depth: 3 })
  }
  if (
    filter &&
    !schema.name.toUpperCase().includes(filter) &&
    !children.length
  ) {
    return
  }
  const name = qualified(database, schema.name)
  lines.push({
    key,
    depth: 1,
    label: schema.name,
    icon: FolderIcon,
    open: isOpen,
    expandable: true,
    dim: schema.name === "INFORMATION_SCHEMA",
    menuTitle: name,
    menu: () => [
      {
        label: "Use in worksheet",
        icon: TargetIcon,
        action: () => switchContext(activeId(), database, schema.name),
      },
      {
        label: "Create table (template)",
        icon: TableIcon,
        action: () =>
          insertIntoEditor(
            `CREATE TABLE ${name}.NEW_TABLE (\n  ID NUMBER AUTOINCREMENT,\n  NAME VARCHAR,\n  CREATED_AT TIMESTAMP_NTZ DEFAULT CURRENT_TIMESTAMP()\n);\n`
          ),
      },
      {
        label: "Create stage…",
        icon: WarehouseIcon,
        action: () => createObject("stage", database, schema.name),
      },
      {
        label: "Copy name",
        icon: CopyIcon,
        action: () => copy(name, "Name copied"),
      },
      null,
      schema.name !== "INFORMATION_SCHEMA" && {
        label: "Drop schema…",
        icon: Trash2Icon,
        danger: true,
        action: () => dropObject(`DROP SCHEMA ${name}`, `schema ${name}`),
      },
    ],
  })
  lines.push(...children)
}

const KIND_ICONS: Record<Kind, LucideIcon> = {
  table: TableIcon,
  view: EyeIcon,
  proc: TerminalIcon,
  func: SquareFunctionIcon,
  stage: WarehouseIcon,
  seq: ListOrderedIcon,
  format: FileCogIcon,
}

function itemLines(
  database: string,
  schema: string,
  item: CatalogObject | Routine | Named,
  kind: Kind,
  options: BuildOptions,
  lines: TreeLine[]
) {
  const { open, filter } = options
  const name = qualified(database, schema, item.name)
  if ("columns" in item) {
    const key = `obj:${database}.${schema}.${item.name}`
    const isOpen =
      open.has(key) || (!!filter && !item.name.toUpperCase().includes(filter))
    const kindLabel = item.kind.replace("_", " ").toLowerCase()
    lines.push({
      key,
      depth: 3,
      label: item.name,
      icon: KIND_ICONS[kind],
      open: isOpen,
      expandable: true,
      title: `${name}\n${kindLabel}${item.rows != null ? ` · ${plural(item.rows, "row")}` : ""}${item.comment ? `\n${item.comment}` : ""}\nDouble-click to insert the name`,
      meta:
        item.rows != null
          ? compactNumber(item.rows)
          : item.kind === "TABLE"
            ? undefined
            : kindLabel,
      onInsert: () => insertIntoEditor(name),
      preview: name,
      menuTitle: name,
      menu: () => objectMenu(database, schema, item, options),
    })
    if (isOpen) {
      for (const column of item.columns) {
        if (
          filter &&
          !item.name.toUpperCase().includes(filter) &&
          !column.name.toUpperCase().includes(filter)
        ) {
          continue
        }
        lines.push({
          key: `col:${key}.${column.name}`,
          depth: 4,
          label: column.name,
          icon: Columns3Icon,
          meta: column.type + (column.nullable ? "" : " NN"),
          title: `${column.name} ${column.type}${column.nullable ? "" : " NOT NULL"}${column.default ? `\nDEFAULT ${column.default}` : ""}${column.comment ? `\n${column.comment}` : ""}`,
          onInsert: () => insertIntoEditor(ident(column.name)),
          menuTitle: column.name,
          menu: () => [
            {
              label: "Insert name",
              icon: TextCursorInputIcon,
              action: () => insertIntoEditor(ident(column.name)),
            },
            {
              label: "Copy name",
              icon: CopyIcon,
              action: () => copy(ident(column.name), "Name copied"),
            },
            {
              label: "Distinct values",
              icon: ListIcon,
              action: () =>
                preview(
                  `SELECT ${ident(column.name)}, COUNT(*) AS "COUNT" FROM ${name} GROUP BY 1 ORDER BY 2 DESC LIMIT 100`,
                  true
                ),
            },
          ],
        })
      }
    }
    return
  }
  const signature = "signature" in item ? (item.signature ?? "") : ""
  const meta =
    kind === "proc" || kind === "func"
      ? signature.replace(/^[^(]*/, "").replace(/ RETURN .*/, "")
      : "detail" in item
        ? (item.detail ?? "")
        : ""
  lines.push({
    key: `${kind}:${database}.${schema}.${item.name}:${signature}`,
    depth: 3,
    label: item.name,
    icon: KIND_ICONS[kind],
    meta: meta || undefined,
    title: [
      name,
      signature,
      "description" in item ? item.description : null,
      "comment" in item ? item.comment : null,
    ]
      .filter(Boolean)
      .join("\n"),
    onInsert: () => insertIntoEditor(kind === "stage" ? `@${name}` : name),
    menuTitle: signature || name,
    menu: () => routineMenu(name, signature, kind, options),
  })
}

function objectMenu(
  database: string,
  schema: string,
  item: CatalogObject,
  options: BuildOptions
): MenuItem[] {
  const name = qualified(database, schema, item.name)
  const isTable = !item.kind.includes("VIEW") && item.kind !== "DYNAMIC_TABLE"
  const dropKind =
    (
      {
        VIEW: "VIEW",
        MATERIALIZED_VIEW: "MATERIALIZED VIEW",
        DYNAMIC_TABLE: "DYNAMIC TABLE",
      } as Record<string, string>
    )[item.kind] ?? "TABLE"
  const columns = item.columns.map((c) => ident(c.name))
  return [
    { label: "Preview data", icon: EyeIcon, action: () => preview(name) },
    {
      label: "Count rows",
      icon: HashIcon,
      action: () => preview(`SELECT COUNT(*) AS ROW_COUNT FROM ${name}`, true),
    },
    {
      label: "Describe",
      icon: InfoIcon,
      action: () =>
        preview(
          `DESCRIBE ${item.kind.includes("VIEW") ? "VIEW" : "TABLE"} ${name}`,
          true
        ),
    },
    null,
    {
      label: "Insert SELECT statement",
      icon: TextCursorInputIcon,
      action: () =>
        insertIntoEditor(
          `SELECT\n  ${columns.join(",\n  ") || "*"}\nFROM ${name}\nLIMIT 100;\n`
        ),
    },
    isTable && {
      label: "Insert INSERT statement",
      icon: TextCursorInputIcon,
      action: () =>
        insertIntoEditor(
          `INSERT INTO ${name} (${columns.join(", ")})\nVALUES (${columns.map(() => "NULL").join(", ")});\n`
        ),
    },
    {
      label: "Insert name",
      icon: TextCursorInputIcon,
      action: () => insertIntoEditor(name),
    },
    {
      label: "Copy name",
      icon: CopyIcon,
      action: () => copy(name, "Name copied"),
    },
    null,
    isTable && {
      label: "Load data from a file…",
      icon: UploadIcon,
      action: () => options.onLoad(database, schema, item),
    },
    isTable && {
      label: "Truncate…",
      icon: EraserIcon,
      danger: true,
      action: () =>
        dropObject(
          `TRUNCATE TABLE ${name}`,
          `every row of ${name}`,
          "Truncate"
        ),
    },
    {
      label: `Drop ${dropKind.toLowerCase()}…`,
      icon: Trash2Icon,
      danger: true,
      action: () =>
        dropObject(
          `DROP ${dropKind} ${name}`,
          `${dropKind.toLowerCase()} ${name}`
        ),
    },
  ]
}

function routineMenu(
  name: string,
  signature: string,
  kind: Kind,
  options: BuildOptions
): MenuItem[] {
  const argTypes = /\(([^)]*)\)/.exec(signature)?.[1] ?? ""
  const copyName: MenuItem = {
    label: "Copy name",
    icon: CopyIcon,
    action: () => copy(kind === "stage" ? `@${name}` : name, "Name copied"),
  }
  switch (kind) {
    case "proc":
      return [
        {
          label: "Insert CALL statement",
          icon: TextCursorInputIcon,
          action: () =>
            insertIntoEditor(
              `CALL ${name}(${argTypes
                .split(",")
                .filter((t) => t.trim())
                .map(() => "NULL")
                .join(", ")});\n`
            ),
        },
        {
          label: "Describe",
          icon: InfoIcon,
          action: () =>
            preview(`DESCRIBE PROCEDURE ${name}(${argTypes})`, true),
        },
        copyName,
        null,
        {
          label: "Drop procedure…",
          icon: Trash2Icon,
          danger: true,
          action: () =>
            dropObject(
              `DROP PROCEDURE ${name}(${argTypes})`,
              `procedure ${name}(${argTypes})`
            ),
        },
      ]
    case "func":
      return [
        {
          label: "Insert call",
          icon: TextCursorInputIcon,
          action: () => insertIntoEditor(`${name}()`),
        },
        copyName,
      ]
    case "stage":
      return [
        {
          label: "List files",
          icon: ListIcon,
          action: () => preview(`LIST @${name}`, true),
        },
        {
          label: "Upload file…",
          icon: UploadIcon,
          action: () => options.onUpload(`@${name}`),
        },
        {
          label: "Query staged files",
          icon: TextCursorInputIcon,
          action: () =>
            insertIntoEditor(
              `SELECT METADATA$FILENAME, $1, $2\nFROM @${name}\nLIMIT 100;\n`
            ),
        },
        copyName,
        null,
        {
          label: "Drop stage…",
          icon: Trash2Icon,
          danger: true,
          action: () => dropObject(`DROP STAGE ${name}`, `stage ${name}`),
        },
      ]
    case "seq":
      return [
        {
          label: "Insert NEXTVAL",
          icon: TextCursorInputIcon,
          action: () => insertIntoEditor(`${name}.NEXTVAL`),
        },
        copyName,
        null,
        {
          label: "Drop sequence…",
          icon: Trash2Icon,
          danger: true,
          action: () => dropObject(`DROP SEQUENCE ${name}`, `sequence ${name}`),
        },
      ]
    case "format":
      return [
        {
          label: "Describe",
          icon: InfoIcon,
          action: () => preview(`DESCRIBE FILE FORMAT ${name}`, true),
        },
        copyName,
        null,
        {
          label: "Drop file format…",
          icon: Trash2Icon,
          danger: true,
          action: () =>
            dropObject(`DROP FILE FORMAT ${name}`, `file format ${name}`),
        },
      ]
    default:
      return [copyName]
  }
}
