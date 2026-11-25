import {
  Columns3Icon,
  DatabaseIcon,
  EyeIcon,
  FolderIcon,
  SquareFunctionIcon,
  TableIcon,
  TypeIcon,
  type LucideIcon,
} from "lucide-react"
import * as React from "react"
import { createPortal } from "react-dom"

import type { Completion, CompletionKind, Word } from "@/lib/completions"
import { wordBefore } from "@/lib/completions"
import { escapeHtml, highlight } from "@/lib/sql"
import { cn } from "@/lib/utils"
import type { EditorHandle } from "@/lib/worksheets"

/**
 * The SQL editor: a textarea over a highlighted copy of its text, with line numbers, the
 * statement at the cursor marked, and autocomplete. It's the UI's one custom component, built
 * from theme tokens to match the stock ones.
 *
 * The textarea is the real editor (its text is transparent), so selection, undo and the
 * browser's text handling all work as usual. Edits the editor makes itself (indenting,
 * completing) go through `execCommand`, which keeps them in the undo history.
 */

const LINE = 20 // px: leading-5
const PAD_X = 12 // px: px-3
const PAD_Y = 10 // px: py-2.5
const LAYER =
  "absolute inset-0 m-0 overflow-hidden border-0 px-3 py-2.5 font-mono text-[13px] leading-5 whitespace-pre"

const ICONS: Record<CompletionKind, LucideIcon> = {
  column: Columns3Icon,
  table: TableIcon,
  view: EyeIcon,
  schema: FolderIcon,
  database: DatabaseIcon,
  keyword: TypeIcon,
  function: SquareFunctionIcon,
}

type Range = [number, number]

type OpenCompletion = {
  word: Word
  items: Completion[]
  active: number
  x: number
  y: number
}

export type EditorCommand = "run" | "runAll" | "save"

export function SqlEditor({
  value,
  onChange,
  onCommand,
  onCaret,
  statement,
  getCompletions,
  placeholder,
  className,
  ref,
}: {
  value: string
  onChange: (value: string) => void
  onCommand: (command: EditorCommand) => void
  /** The cursor moved or the text changed. */
  onCaret?: () => void
  /** The statement to mark (the one the cursor is in), as UTF-16 offsets. */
  statement: Range | null
  getCompletions: (
    word: Word,
    text: string,
    offset: number
  ) => Promise<Completion[]>
  placeholder?: string
  className?: string
  ref?: React.Ref<EditorHandle>
}) {
  const textarea = React.useRef<HTMLTextAreaElement>(null)
  const code = React.useRef<HTMLPreElement>(null)
  const marks = React.useRef<HTMLPreElement>(null)
  const gutter = React.useRef<HTMLPreElement>(null)
  const list = React.useRef<HTMLDivElement>(null)
  const [completion, setCompletion] = React.useState<OpenCompletion | null>(
    null
  )
  // What event handlers read: the latest props and state, and the layers' rendered state.
  const latest = React.useRef({ onChange, onCommand, onCaret, getCompletions })
  const live = React.useRef({
    completion: null as OpenCompletion | null,
    statement: null as Range | null,
    editing: false,
    lines: 0,
    currentLine: 0,
    charWidth: 0,
  })

  React.useLayoutEffect(() => {
    latest.current = { onChange, onCommand, onCaret, getCompletions }
    live.current.completion = completion
  })

  const ta = () => textarea.current as HTMLTextAreaElement

  const syncScroll = React.useCallback(() => {
    const { scrollTop, scrollLeft } = ta()
    for (const layer of [code.current, marks.current]) {
      if (!layer) continue
      layer.scrollTop = scrollTop
      layer.scrollLeft = scrollLeft
    }
    if (gutter.current) gutter.current.scrollTop = scrollTop
  }, [])

  const renderMarks = React.useCallback(() => {
    const el = marks.current
    if (!el) return
    const text = ta().value
    const range = live.current.statement
    if (
      !range ||
      range[1] > text.length ||
      ta().selectionStart !== ta().selectionEnd
    ) {
      el.textContent = ""
      return
    }
    el.innerHTML = `${escapeHtml(text.slice(0, range[0]))}<mark class="rounded-xs bg-primary/10 text-transparent ring-2 ring-primary/10">${escapeHtml(text.slice(range[0], range[1]))}</mark>${escapeHtml(text.slice(range[1]))}\n `
    syncScroll()
  }, [syncScroll])

  const renderGutter = React.useCallback(() => {
    const el = gutter.current
    if (!el) return
    const text = ta().value
    let lines = 1
    for (let i = 0; i < text.length; i++) {
      if (text.charCodeAt(i) === 10) lines++
    }
    const current = text.slice(0, ta().selectionStart).split("\n").length
    if (lines !== live.current.lines || current !== live.current.currentLine) {
      live.current.lines = lines
      live.current.currentLine = current
      const numbers: string[] = []
      for (let i = 1; i <= lines; i++) {
        numbers.push(
          i === current ? `<span class="text-foreground">${i}</span>` : `${i}`
        )
      }
      el.innerHTML = `${numbers.join("\n")}\n `
    }
    syncScroll()
  }, [syncScroll])

  const renderText = React.useCallback(() => {
    if (code.current) code.current.innerHTML = `${highlight(ta().value)}\n `
    renderMarks()
    renderGutter()
    syncScroll()
  }, [renderMarks, renderGutter, syncScroll])

  const closeCompletion = React.useCallback(() => {
    live.current.completion = null
    setCompletion(null)
  }, [])

  /** Replaces a range, keeping the browser's undo history. */
  const replace = React.useCallback(
    (
      start: number,
      end: number,
      text: string,
      selectStart = start + text.length,
      selectEnd = selectStart
    ) => {
      const el = ta()
      el.focus({ preventScroll: true })
      el.setSelectionRange(start, end)
      live.current.editing = true
      try {
        const done =
          text === ""
            ? document.execCommand("delete")
            : document.execCommand("insertText", false, text)
        if (!done) {
          el.setRangeText(text, start, end, "end")
          el.dispatchEvent(new Event("input", { bubbles: true }))
        }
      } finally {
        live.current.editing = false
      }
      el.setSelectionRange(selectStart, selectEnd)
      renderText()
    },
    [renderText]
  )

  React.useImperativeHandle(
    ref,
    () => ({
      focus: () => ta().focus({ preventScroll: true }),
      focused: () => document.activeElement === textarea.current,
      insert: (text) => replace(ta().selectionStart, ta().selectionEnd, text),
      selection: () => {
        const { selectionStart: start, selectionEnd: end, value: text } = ta()
        return { start, end, text: text.slice(start, end) }
      },
    }),
    [replace]
  )

  // The first render, and text changed from outside (opening a saved query).
  React.useLayoutEffect(() => {
    const el = ta()
    if (el.value === value) {
      // Typing: onInput has drawn it. Only the first render draws here.
      if (!live.current.lines) renderText()
      return
    }
    if (el.value) {
      // Through replace(), so undo brings the old text back.
      replace(0, el.value.length, value, 0, 0)
    } else {
      el.value = value
      renderText()
    }
    el.scrollTop = 0
  }, [value, replace, renderText])

  React.useEffect(() => {
    live.current.statement = statement
    renderMarks()
  }, [statement, renderMarks])

  const caret = React.useCallback(() => {
    renderGutter()
    renderMarks()
    latest.current.onCaret?.()
  }, [renderGutter, renderMarks])

  React.useEffect(() => {
    const onSelection = () => {
      if (document.activeElement === textarea.current) caret()
    }
    document.addEventListener("selectionchange", onSelection)
    return () => document.removeEventListener("selectionchange", onSelection)
  }, [caret])

  // Re-measure the character width when the font loads or changes.
  React.useEffect(() => {
    void document.fonts?.ready.then(() => {
      live.current.charWidth = 0
    })
  }, [])

  /** Viewport coordinates just below a text offset (the editor never wraps lines). */
  function caretPosition(offset: number) {
    const el = ta()
    if (!live.current.charWidth && code.current) {
      const probe = document.createElement("span")
      probe.style.visibility = "hidden"
      probe.style.position = "absolute"
      probe.textContent = "x".repeat(100)
      code.current.append(probe)
      live.current.charWidth = probe.getBoundingClientRect().width / 100
      probe.remove()
    }
    const before = el.value.slice(0, offset)
    const line = before.split("\n").length - 1
    const column = before.length - before.lastIndexOf("\n") - 1
    const rect = el.getBoundingClientRect()
    return {
      x: rect.left + PAD_X + column * live.current.charWidth - el.scrollLeft,
      y: rect.top + PAD_Y + (line + 1) * LINE - el.scrollTop + 2,
    }
  }

  async function complete(explicit: boolean) {
    const el = ta()
    const word = wordBefore(el.value, el.selectionStart)
    if (!explicit && !word.path.length && word.prefix.length < 2) {
      closeCompletion()
      return
    }
    const items = await latest.current.getCompletions(
      word,
      el.value,
      el.selectionStart
    )
    const now = wordBefore(el.value, el.selectionStart)
    if (now.start !== word.start || now.prefix !== word.prefix) return
    const prefix = word.prefix.replace(/^"/, "").toUpperCase()
    const matches = items
      .filter((item) => item.label.toUpperCase().startsWith(prefix))
      .sort((a, b) => a.rank - b.rank || a.label.localeCompare(b.label))
      .slice(0, 60)
    if (
      !matches.length ||
      (!explicit &&
        matches.length === 1 &&
        matches[0].label.toUpperCase() === prefix)
    ) {
      closeCompletion()
      return
    }
    const open = {
      word,
      items: matches,
      active: 0,
      ...caretPosition(word.start),
    }
    live.current.completion = open
    setCompletion(open)
  }

  function accept(index: number) {
    const open = live.current.completion
    if (!open) return
    const item = open.items[index]
    closeCompletion()
    replace(open.word.start, ta().selectionStart, item.insert ?? item.label)
  }

  function completionKey(event: React.KeyboardEvent) {
    const open = live.current.completion
    if (!open) return false
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault()
      const n = open.items.length
      const next = {
        ...open,
        active: (open.active + (event.key === "ArrowDown" ? 1 : n - 1)) % n,
      }
      live.current.completion = next
      setCompletion(next)
      return true
    }
    if (event.key === "Enter" || event.key === "Tab") {
      if (event.metaKey || event.ctrlKey) return false
      event.preventDefault()
      accept(open.active)
      return true
    }
    if (event.key === "Escape") {
      event.preventDefault()
      event.stopPropagation()
      closeCompletion()
      return true
    }
    return false
  }

  /** The lines a selection touches: [start of the first line, end of the last]. */
  function selectedLines() {
    const { selectionStart: start, selectionEnd: end, value: text } = ta()
    const from = text.lastIndexOf("\n", start - 1) + 1
    let to = text.indexOf(
      "\n",
      end > start && text[end - 1] === "\n" ? end - 1 : end
    )
    if (to === -1) to = text.length
    return [from, to] as const
  }

  function indent(outdent: boolean) {
    const { selectionStart: start, selectionEnd: end, value: text } = ta()
    if (!outdent && !text.slice(start, end).includes("\n")) {
      replace(start, end, "  ")
      return
    }
    const [from, to] = selectedLines()
    const result = text
      .slice(from, to)
      .split("\n")
      .map((line) => (outdent ? line.replace(/^( {1,2}|\t)/, "") : `  ${line}`))
      .join("\n")
    replace(from, to, result, from, from + result.length)
  }

  function toggleComment() {
    const [from, to] = selectedLines()
    const lines = ta().value.slice(from, to).split("\n")
    const commented = lines
      .filter((l) => l.trim())
      .every((l) => /^\s*--/.test(l))
    const result = lines
      .map((line) => {
        if (!line.trim()) return line
        return commented
          ? line.replace(/^(\s*)-- ?/, "$1")
          : line.replace(/^(\s*)/, "$1-- ")
      })
      .join("\n")
    replace(from, to, result, from, from + result.length)
  }

  function onKeyDown(event: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (event.nativeEvent.isComposing) return
    const mod = event.metaKey || event.ctrlKey
    if (completionKey(event)) return
    if (mod && event.key === "Enter") {
      event.preventDefault()
      latest.current.onCommand(event.shiftKey ? "runAll" : "run")
    } else if (mod && (event.key === "s" || event.key === "S")) {
      event.preventDefault()
      latest.current.onCommand("save")
    } else if (mod && event.key === "/") {
      event.preventDefault()
      toggleComment()
    } else if (event.ctrlKey && event.code === "Space") {
      event.preventDefault()
      void complete(true)
    } else if (event.key === "Tab" && !mod && !event.altKey) {
      event.preventDefault()
      indent(event.shiftKey)
    } else if (event.key === "Enter" && !event.shiftKey && !event.altKey) {
      // Keep the line's indentation.
      event.preventDefault()
      const { selectionStart: start, selectionEnd: end, value: text } = ta()
      const lineStart = text.lastIndexOf("\n", start - 1) + 1
      const indentation = /^[ \t]*/.exec(text.slice(lineStart, start))?.[0]
      replace(start, end, `\n${indentation ?? ""}`)
    }
  }

  function onInput(event: React.FormEvent<HTMLTextAreaElement>) {
    renderText()
    latest.current.onChange(ta().value)
    const input = event.nativeEvent as InputEvent
    if (
      !live.current.editing &&
      input.inputType === "insertText" &&
      /^[\w$.]$/.test(input.data ?? "")
    ) {
      void complete(false)
    } else {
      closeCompletion()
    }
  }

  // Keep the popup on screen, and the active item in view.
  React.useLayoutEffect(() => {
    const el = list.current
    if (!el || !completion) return
    const { width, height } = el.getBoundingClientRect()
    el.style.left = `${Math.max(8, Math.min(completion.x, innerWidth - width - 8))}px`
    el.style.top = `${completion.y + height > innerHeight - 8 ? completion.y - height - LINE - 2 : completion.y}px`
    el.children[completion.active]?.scrollIntoView({ block: "nearest" })
  }, [completion])

  return (
    <div
      className={cn(
        "relative flex min-h-0 bg-background text-foreground",
        className
      )}
    >
      <pre
        ref={gutter}
        aria-hidden
        className="m-0 min-w-11 shrink-0 overflow-hidden border-r bg-muted/40 py-2.5 pr-2 pl-3 text-right font-mono text-[13px] leading-5 text-muted-foreground select-none"
      />
      <div className="relative min-w-0 flex-1">
        <pre
          ref={marks}
          aria-hidden
          className={cn(LAYER, "text-transparent")}
        />
        <pre ref={code} aria-hidden className={LAYER} />
        <textarea
          ref={textarea}
          aria-label="SQL"
          spellCheck={false}
          autoCapitalize="off"
          autoComplete="off"
          autoCorrect="off"
          wrap="off"
          placeholder={placeholder}
          defaultValue={value}
          className={cn(
            LAYER,
            "resize-none overflow-auto bg-transparent text-transparent caret-foreground outline-none selection:bg-primary/25 selection:text-transparent placeholder:text-muted-foreground"
          )}
          onInput={onInput}
          onKeyDown={onKeyDown}
          onKeyUp={caret}
          onClick={caret}
          onScroll={syncScroll}
          onBlur={() => setTimeout(closeCompletion, 150)}
        />
      </div>
      {completion &&
        createPortal(
          <div
            ref={list}
            role="listbox"
            aria-label="Completions"
            className="fixed z-50 max-h-60 w-max max-w-md min-w-56 overflow-y-auto rounded-lg bg-popover p-1 text-popover-foreground shadow-md ring-1 ring-foreground/10"
            style={{ left: completion.x, top: completion.y }}
          >
            {completion.items.map((item, i) => {
              const Icon = ICONS[item.kind]
              return (
                <div
                  key={`${item.label}|${item.detail}`}
                  role="option"
                  aria-selected={i === completion.active}
                  className="flex cursor-default items-center gap-2 rounded-md px-1.5 py-1 text-sm aria-selected:bg-accent aria-selected:text-accent-foreground"
                  onMouseDown={(e) => {
                    e.preventDefault()
                    accept(i)
                  }}
                >
                  <Icon className="size-3.5 shrink-0 text-muted-foreground" />
                  <span className="truncate font-mono text-xs">
                    {item.label}
                  </span>
                  <span className="ml-auto pl-4 text-xs text-muted-foreground">
                    {item.detail}
                  </span>
                </div>
              )
            })}
          </div>,
          document.body
        )}
    </div>
  )
}
