import {
  ArrowDownIcon,
  ArrowUpIcon,
  Columns3Icon,
  CopyIcon,
  EyeIcon,
  FilterIcon,
} from "lucide-react"
import * as React from "react"
import { toast } from "sonner"

import { MenuItems, type MenuItem } from "@/components/menu-items"
import { ContextMenu, ContextMenuTrigger } from "@/components/ui/context-menu"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { isJson, isNumeric, rowObject, typeLabel } from "@/lib/format"
import type { Column, Row } from "@/lib/types"
import { cn } from "@/lib/utils"

/** Rows are a fixed height, so only those in view are rendered. */
const ROW_H = 28

export type Sort = { column: number; direction: 1 | -1 } | null
export type Cell = { row: number; column: number }

export async function copy(text: string, what: string) {
  try {
    await navigator.clipboard.writeText(text)
    toast(what)
  } catch {
    toast.error("Couldn't copy to the clipboard")
  }
}

/** Starting widths from the column names and the first rows' text. */
export function measure(columns: Column[], text: (string | null)[][]) {
  const widths = columns.map(
    (column) =>
      Math.max(column.name.length, typeLabel(column).length * 0.85) * 7.4 + 22
  )
  for (const row of text.slice(0, 200)) {
    row.forEach((value, c) => {
      const length = value == null ? 4 : Math.min(value.length, 60)
      widths[c] = Math.max(widths[c], length * 7.3 + 18)
    })
  }
  return widths.map((w) => Math.round(Math.min(Math.max(w, 64), 420)))
}

/**
 * A result's rows in a stock table that renders only the rows in view. Click a cell to select
 * it, double-click (or Enter) to see all of it, right-click for actions.
 */
export function ResultGrid({
  columns,
  rows,
  view,
  textOf,
  total,
  sort,
  onSort,
  onOpen,
  onFilterRequest,
  extraActions,
  onNearEnd,
  footer,
}: {
  columns: Column[]
  rows: Row[]
  /** Indexes into `rows`, filtered and sorted. */
  view: number[]
  textOf: (index: number) => (string | null)[]
  total: number
  sort: Sort
  onSort: (column: number) => void
  onOpen: (cell: Cell) => void
  onFilterRequest: (value: string) => void
  extraActions?: (cell: Cell, text: (string | null)[]) => MenuItem[]
  /** Scrolled close to the last row. */
  onNearEnd: () => void
  footer: string
}) {
  const scroller = React.useRef<HTMLDivElement>(null)
  const head = React.useRef<HTMLTableSectionElement>(null)
  const [widths, setWidths] = React.useState(() =>
    measure(
      columns,
      view.slice(0, 200).map((i) => textOf(i))
    )
  )
  const [frame, setFrame] = React.useState({ top: 0, height: 600, head: 40 })
  const [selected, setSelected] = React.useState<Cell | null>(null)
  const [menuCell, setMenuCell] = React.useState<Cell | null>(null)

  const measureFrame = React.useCallback(() => {
    const el = scroller.current
    if (!el) return
    setFrame({
      top: el.scrollTop,
      height: el.clientHeight,
      head: head.current?.offsetHeight ?? 40,
    })
  }, [])

  React.useEffect(() => {
    const el = scroller.current
    if (!el) return
    const observer = new ResizeObserver(measureFrame)
    observer.observe(el)
    return () => observer.disconnect()
  }, [measureFrame])

  const rnWidth = Math.max(String(total).length * 8 + 18, 40)
  const tableWidth = rnWidth + widths.reduce((a, b) => a + b, 0)
  const offset = Math.max(0, frame.top - frame.head)
  const first = Math.max(0, Math.floor(offset / ROW_H) - 8)
  const last = Math.min(
    view.length,
    Math.ceil((offset + frame.height) / ROW_H) + 8
  )

  function onScroll() {
    measureFrame()
    const el = scroller.current
    if (el && el.scrollTop + el.clientHeight > el.scrollHeight - ROW_H * 40) {
      onNearEnd()
    }
  }

  function cellAt(event: React.MouseEvent): Cell | null {
    const target = event.target as HTMLElement
    const tr = target.closest<HTMLElement>("tr[data-r]")
    if (!tr) return null
    const td = target.closest<HTMLElement>("td[data-c]")
    return { row: Number(tr.dataset.r), column: Number(td?.dataset.c ?? 0) }
  }

  function startResize(event: React.MouseEvent, c: number) {
    event.preventDefault()
    event.stopPropagation()
    const startX = event.clientX
    const start = widths[c]
    const move = (e: MouseEvent) =>
      setWidths((w) =>
        w.map((width, i) =>
          i === c ? Math.max(48, start + e.clientX - startX) : width
        )
      )
    const up = () => {
      removeEventListener("mousemove", move)
      removeEventListener("mouseup", up)
      document.body.style.cursor = ""
    }
    document.body.style.cursor = "col-resize"
    addEventListener("mousemove", move)
    addEventListener("mouseup", up)
  }

  function onKeyDown(event: React.KeyboardEvent) {
    if (!selected) return
    if ((event.metaKey || event.ctrlKey) && event.key === "c") {
      event.preventDefault()
      void copy(textOf(selected.row)[selected.column] ?? "", "Value copied")
      return
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault()
      onOpen(selected)
      return
    }
    const moves: Record<string, [number, number]> = {
      ArrowUp: [-1, 0],
      ArrowDown: [1, 0],
      ArrowLeft: [0, -1],
      ArrowRight: [0, 1],
    }
    const move = moves[event.key]
    if (!move) return
    event.preventDefault()
    const position = view.indexOf(selected.row)
    const v = Math.min(Math.max(position + move[0], 0), view.length - 1)
    const column = Math.min(
      Math.max(selected.column + move[1], 0),
      columns.length - 1
    )
    setSelected({ row: view[v], column })
    const el = scroller.current
    if (!el) return
    const top = v * ROW_H
    if (top < el.scrollTop) el.scrollTop = top
    else if (top + ROW_H + frame.head > el.scrollTop + el.clientHeight) {
      el.scrollTop = top + ROW_H + frame.head - el.clientHeight
    }
  }

  const menu: MenuItem[] = (() => {
    if (!menuCell) return []
    const text = textOf(menuCell.row)
    const column = columns[menuCell.column]
    return [
      {
        label: "Copy value",
        icon: CopyIcon,
        action: () => copy(text[menuCell.column] ?? "", "Value copied"),
      },
      { label: "View value", icon: EyeIcon, action: () => onOpen(menuCell) },
      null,
      {
        label: "Copy row as TSV",
        icon: CopyIcon,
        action: () => copy(text.map((t) => t ?? "").join("\t"), "Row copied"),
      },
      {
        label: "Copy row as JSON",
        icon: CopyIcon,
        action: () =>
          copy(JSON.stringify(rowObject(columns, text), null, 2), "Row copied"),
      },
      {
        label: "Copy column name",
        icon: Columns3Icon,
        action: () => copy(column.name, "Column name copied"),
      },
      null,
      {
        label: "Filter rows to this value",
        icon: FilterIcon,
        action: () => onFilterRequest(text[menuCell.column] ?? "NULL"),
      },
      ...(extraActions?.(menuCell, text) ?? []),
    ]
  })()

  return (
    <div
      ref={scroller}
      tabIndex={0}
      aria-label="Result rows"
      className="relative min-h-0 flex-1 overflow-auto font-mono text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:ring-inset [&>[data-slot=table-container]]:overflow-visible"
      onScroll={onScroll}
      onKeyDown={onKeyDown}
    >
      <Table
        className="table-fixed text-xs"
        style={{ width: tableWidth, minWidth: tableWidth }}
      >
        <colgroup>
          <col style={{ width: rnWidth }} />
          {widths.map((w, c) => (
            <col key={c} style={{ width: w }} />
          ))}
        </colgroup>
        <TableHeader
          ref={head}
          className="sticky top-0 z-10 bg-background font-sans shadow-[0_1px_0_var(--color-border)] [&_tr]:border-0"
        >
          <TableRow className="hover:bg-transparent">
            <TableHead className="h-10" />
            {columns.map((column, c) => (
              <TableHead
                key={c}
                title={`${column.name}\n${typeLabel(column)}${column.nullable === false ? " NOT NULL" : ""}\nClick to sort`}
                className="relative h-10 cursor-pointer overflow-hidden px-2 leading-tight select-none hover:bg-muted/50"
                onClick={() => onSort(c)}
              >
                <div className="flex items-center gap-1">
                  <span className="truncate">{column.name}</span>
                  {sort?.column === c &&
                    (sort.direction > 0 ? (
                      <ArrowUpIcon className="size-3 shrink-0" />
                    ) : (
                      <ArrowDownIcon className="size-3 shrink-0" />
                    ))}
                </div>
                <div className="truncate text-[10px] font-normal text-muted-foreground">
                  {typeLabel(column)}
                </div>
                <span
                  aria-hidden
                  className="absolute inset-y-0 right-0 w-1.5 cursor-col-resize hover:bg-primary/40"
                  onMouseDown={(e) => startResize(e, c)}
                  onClick={(e) => e.stopPropagation()}
                />
              </TableHead>
            ))}
          </TableRow>
        </TableHeader>
        <ContextMenu>
          <ContextMenuTrigger
            render={
              <TableBody
                onClick={(e) => {
                  const cell = cellAt(e)
                  if (cell) setSelected(cell)
                }}
                onDoubleClick={(e) => {
                  const cell = cellAt(e)
                  if (cell) onOpen(cell)
                }}
                onContextMenu={(e) => {
                  const cell = cellAt(e)
                  setMenuCell(cell)
                  if (cell) setSelected(cell)
                }}
              />
            }
          >
            {first > 0 && <tr aria-hidden style={{ height: first * ROW_H }} />}
            {view.slice(first, last).map((index, i) => {
              const v = first + i
              const raw = rows[index]
              return (
                <TableRow
                  key={index}
                  data-r={index}
                  className={cn("h-7 border-0", v % 2 === 1 && "bg-muted/30")}
                >
                  <TableCell className="overflow-hidden py-0 text-right text-muted-foreground tabular-nums shadow-[inset_-1px_-1px_0_var(--color-border)]">
                    {index + 1}
                  </TableCell>
                  {textOf(index).map((value, c) => {
                    const column = columns[c]
                    let shown = value ?? "NULL"
                    if (value != null && isJson(column)) {
                      shown = value.replace(/\n\s*/g, " ")
                    } else if (value?.includes("\n")) {
                      shown = value.replace(/\r?\n/g, "↵ ")
                    }
                    const isSelected =
                      selected?.row === index && selected.column === c
                    return (
                      <TableCell
                        key={c}
                        data-c={c}
                        data-selected={isSelected || undefined}
                        className={cn(
                          "overflow-hidden py-0 text-ellipsis shadow-[inset_-1px_-1px_0_var(--color-border)] data-selected:bg-primary/10 data-selected:outline-2 data-selected:-outline-offset-2 data-selected:outline-ring",
                          value == null && "text-muted-foreground italic",
                          value != null &&
                            isNumeric(column) &&
                            "text-right tabular-nums",
                          column.type === "boolean" &&
                            raw[c] === "1" &&
                            "text-primary"
                        )}
                      >
                        {shown.length > 500 ? `${shown.slice(0, 500)}…` : shown}
                      </TableCell>
                    )
                  })}
                </TableRow>
              )
            })}
            {last < view.length && (
              <tr
                aria-hidden
                style={{ height: (view.length - last) * ROW_H }}
              />
            )}
          </ContextMenuTrigger>
          {menuCell && <MenuItems as="context" items={menu} />}
        </ContextMenu>
      </Table>
      {footer && (
        <div className="sticky left-0 px-3 py-2 font-sans text-xs text-muted-foreground">
          {footer}
        </div>
      )}
    </div>
  )
}
