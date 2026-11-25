import {
  BookmarkIcon,
  DownloadIcon,
  PencilIcon,
  SearchIcon,
  Trash2Icon,
  UploadIcon,
} from "lucide-react"
import * as React from "react"
import { toast } from "sonner"

import { Button } from "@/components/ui/button"
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty"
import {
  InputGroup,
  InputGroupAddon,
  InputGroupInput,
} from "@/components/ui/input-group"
import {
  Item,
  ItemActions,
  ItemContent,
  ItemDescription,
  ItemTitle,
} from "@/components/ui/item"
import { ScrollArea } from "@/components/ui/scroll-area"
import { message } from "@/lib/api"
import { confirm, prompt } from "@/lib/dialogs"
import { MOD, download, relativeTime } from "@/lib/format"
import { importSaved, removeSaved, saved, upsertSaved } from "@/lib/saved"
import { useStore } from "@/lib/store"
import type { SavedQuery } from "@/lib/types"
import { openSaved, syncSaved } from "@/lib/worksheets"

/** Saved queries, kept in this browser. Clicking one opens it in a worksheet. */
export function SavedQueries() {
  const items = useStore(saved)
  const [search, setSearch] = React.useState("")
  const importInput = React.useRef<HTMLInputElement>(null)
  const needle = search.trim().toLowerCase()
  const shown = items.filter(
    (q) =>
      !needle ||
      q.name.toLowerCase().includes(needle) ||
      q.sql.toLowerCase().includes(needle)
  )

  return (
    <>
      <div className="shrink-0 p-2">
        <InputGroup className="h-8">
          <InputGroupAddon>
            <SearchIcon />
          </InputGroupAddon>
          <InputGroupInput
            type="search"
            placeholder="Search saved queries"
            aria-label="Search saved queries"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
        </InputGroup>
      </div>
      <ScrollArea className="min-h-0 flex-1">
        {!items.length ? (
          <Empty className="px-4">
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <BookmarkIcon />
              </EmptyMedia>
              <EmptyTitle>No saved queries yet</EmptyTitle>
              <EmptyDescription>
                Press {MOD}+S in a worksheet to save its SQL here. Saved queries
                live in this browser's storage.
              </EmptyDescription>
            </EmptyHeader>
          </Empty>
        ) : (
          <div className="flex flex-col gap-0.5 px-1.5 pb-2">
            {shown.map((query) => (
              <SavedItem key={query.id} query={query} />
            ))}
            {!shown.length && (
              <p className="px-2 py-1.5 text-xs text-muted-foreground">
                No saved queries match.
              </p>
            )}
          </div>
        )}
      </ScrollArea>
      <div className="flex shrink-0 gap-2 border-t p-2">
        <Button
          variant="outline"
          size="sm"
          title="Download every saved query as JSON"
          disabled={!items.length}
          onClick={() =>
            download(
              "localflake-saved-queries.json",
              JSON.stringify(items, null, 2),
              "application/json"
            )
          }
        >
          <DownloadIcon data-icon="inline-start" />
          Export
        </Button>
        <Button
          variant="outline"
          size="sm"
          title="Add saved queries from an exported JSON file"
          onClick={() => importInput.current?.click()}
        >
          <UploadIcon data-icon="inline-start" />
          Import
        </Button>
        <input
          ref={importInput}
          type="file"
          accept=".json,application/json"
          className="hidden"
          onChange={(e) => {
            const file = e.target.files?.[0]
            e.target.value = ""
            if (!file) return
            importSaved(file).then(
              (summary) => toast(summary),
              (err: unknown) => toast.error(`Can't import: ${message(err)}`)
            )
          }}
        />
      </div>
    </>
  )
}

function SavedItem({ query }: { query: SavedQuery }) {
  async function rename() {
    const name = await prompt({
      title: "Rename saved query",
      label: "Name",
      value: query.name,
      confirmLabel: "Rename",
    })
    if (!name) return
    upsertSaved({ id: query.id, name })
    syncSaved(query.id, { name })
  }

  async function remove() {
    const ok = await confirm({
      title: "Delete saved query",
      message: `Delete “${query.name}”? Open worksheets keep their SQL.`,
      confirmLabel: "Delete",
      danger: true,
    })
    if (!ok) return
    removeSaved(query.id)
    syncSaved(query.id, null)
  }

  return (
    <Item
      size="xs"
      className="group/saved relative items-start hover:bg-muted"
      title={query.sql}
    >
      <ItemContent className="min-w-0">
        <ItemTitle className="w-full">
          <button
            type="button"
            className="truncate text-left outline-none after:absolute after:inset-0 after:rounded-lg focus-visible:underline"
            onClick={() => openSaved(query)}
          >
            {query.name}
          </button>
          <span className="ml-auto shrink-0 text-xs font-normal text-muted-foreground group-hover/saved:invisible">
            {relativeTime(query.updatedAt)}
          </span>
        </ItemTitle>
        <ItemDescription className="line-clamp-2 font-mono text-[11px]">
          {query.sql.replace(/\s+/g, " ").slice(0, 200)}
        </ItemDescription>
      </ItemContent>
      <ItemActions className="absolute top-1.5 right-1.5 z-10 hidden gap-0.5 group-hover/saved:flex focus-within:flex">
        <Button
          variant="ghost"
          size="icon-xs"
          aria-label={`Rename ${query.name}`}
          onClick={() => void rename()}
        >
          <PencilIcon />
        </Button>
        <Button
          variant="ghost"
          size="icon-xs"
          aria-label={`Delete ${query.name}`}
          onClick={() => void remove()}
        >
          <Trash2Icon />
        </Button>
      </ItemActions>
    </Item>
  )
}
