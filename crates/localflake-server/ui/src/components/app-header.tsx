import { DatabaseIcon, KeyboardIcon, SnowflakeIcon } from "lucide-react"
import * as React from "react"
import { Link, matchPath, useLocation, useRouteLoaderData } from "react-router"

import { ModeToggle } from "@/components/mode-toggle"
import { ShortcutsDialog } from "@/components/shortcuts-dialog"
import { Badge } from "@/components/ui/badge"
import { Button, buttonVariants } from "@/components/ui/button"
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import { history, sessions } from "@/lib/live"
import { useStore } from "@/lib/store"
import type { HistoryRecord, Info } from "@/lib/types"

/** Each nav entry, and the paths that show it as current. */
const NAV = [
  { to: "/", label: "Worksheets", paths: ["/"] },
  { to: "/history", label: "Query history", paths: ["/history/*"] },
  { to: "/sessions", label: "Sessions", paths: ["/sessions"] },
]

const countRunning = (records: ReadonlyMap<string, HistoryRecord>) => {
  let n = 0
  for (const r of records.values()) if (r.status === "running") n++
  return n
}

export function AppHeader() {
  const info = useRouteLoaderData("root") as Info
  const { pathname } = useLocation()
  const running = useStore(history, (s) => countRunning(s.records))
  const sessionCount = useStore(sessions, (s) => s?.length ?? null)
  const [shortcuts, setShortcuts] = React.useState(false)

  React.useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null
      if (
        e.key === "?" &&
        !target?.closest("input, textarea, select, [contenteditable]") &&
        !document.querySelector("[role=dialog], [role=alertdialog]")
      ) {
        setShortcuts(true)
      }
    }
    document.addEventListener("keydown", onKey)
    return () => document.removeEventListener("keydown", onKey)
  }, [])

  return (
    <header className="flex h-12 shrink-0 items-center gap-4 border-b px-4">
      <Link to="/" className="flex items-center gap-2 text-sm font-semibold">
        <span className="flex size-6 items-center justify-center rounded-md bg-primary text-primary-foreground">
          <SnowflakeIcon className="size-3.5" />
        </span>
        localflake
        <Badge variant="secondary" className="font-mono font-normal">
          {info.version}
        </Badge>
      </Link>
      <nav className="flex items-center gap-1">
        {NAV.map(({ to, label, paths }) => {
          const current = paths.some((p) => matchPath(p, pathname))
          return (
            <Link
              key={to}
              to={to}
              aria-current={current ? "page" : undefined}
              className={buttonVariants({
                variant: current ? "secondary" : "ghost",
                size: "sm",
              })}
            >
              {label}
              {to === "/history" && running > 0 && (
                <Badge className="animate-pulse">{running} running</Badge>
              )}
              {to === "/sessions" && sessionCount != null && (
                <Badge variant="outline" className="tabular-nums">
                  {sessionCount}
                </Badge>
              )}
            </Link>
          )
        })}
      </nav>
      <div className="ml-auto flex items-center gap-2">
        <Tooltip>
          <TooltipTrigger
            render={
              <Badge
                variant="outline"
                className="max-w-80 font-normal text-muted-foreground"
              />
            }
          >
            <DatabaseIcon data-icon="inline-start" />
            <span className="truncate">
              {info.dataDir ? `Persistent · ${info.dataDir}` : "In memory"}
            </span>
          </TooltipTrigger>
          <TooltipContent>
            {info.dataDir
              ? "Databases are stored in this directory and survive restarts."
              : "Everything is lost when localflake stops. Start it with --data-dir to keep databases."}
          </TooltipContent>
        </Tooltip>
        <Tooltip>
          <TooltipTrigger
            render={
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label="Keyboard shortcuts"
                onClick={() => setShortcuts(true)}
              />
            }
          >
            <KeyboardIcon />
          </TooltipTrigger>
          <TooltipContent>Keyboard shortcuts (?)</TooltipContent>
        </Tooltip>
        <ModeToggle />
      </div>
      <ShortcutsDialog open={shortcuts} onOpenChange={setShortcuts} />
    </header>
  )
}
