import { RotateCwIcon, TriangleAlertIcon } from "lucide-react"
import {
  isRouteErrorResponse,
  Outlet,
  useMatch,
  useRouteError,
} from "react-router"

import { AppHeader } from "@/components/app-header"
import { DialogHost } from "@/components/dialog-host"
import { useTheme } from "@/components/theme-provider"
import { Button } from "@/components/ui/button"
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty"
import { Toaster } from "@/components/ui/sonner"
import { Spinner } from "@/components/ui/spinner"
import { api } from "@/lib/api"
import type { Info } from "@/lib/types"
import { cn } from "@/lib/utils"
import { WorksheetsView } from "@/routes/worksheets"

export const rootLoader = () => api<Info>("/info")

export function Root() {
  const { theme } = useTheme()
  // The worksheets stay mounted while other pages show, like tabs: their editors keep their
  // undo history, and their results their scroll position, sort and filter.
  const onWorksheets = useMatch({ path: "/", end: true }) != null
  return (
    <div className="flex h-svh flex-col">
      <AppHeader />
      <main className="relative min-h-0 flex-1">
        <WorksheetsView className={cn(!onWorksheets && "hidden")} />
        {!onWorksheets && (
          <div className="h-full overflow-y-auto">
            <Outlet />
          </div>
        )}
      </main>
      <DialogHost />
      <Toaster theme={theme} />
    </div>
  )
}

/** Shown while the first page loads. */
export function Loading() {
  return (
    <div className="flex h-svh items-center justify-center">
      <Spinner className="size-6 text-muted-foreground" />
    </div>
  )
}

export function RouteError() {
  const error = useRouteError()
  const unreachable =
    error instanceof TypeError ||
    (error instanceof Error && error.message.includes("isn't responding"))
  const message = isRouteErrorResponse(error)
    ? `${error.status} ${error.statusText}`
    : error instanceof Error
      ? error.message
      : String(error)
  return (
    <Empty className="h-full">
      <EmptyHeader>
        <EmptyMedia variant="icon">
          <TriangleAlertIcon />
        </EmptyMedia>
        <EmptyTitle>
          {unreachable ? "localflake isn't answering" : "Something went wrong"}
        </EmptyTitle>
        <EmptyDescription>
          {unreachable
            ? "Check that it's still running, then reload."
            : message}
        </EmptyDescription>
      </EmptyHeader>
      <EmptyContent>
        <Button variant="outline" onClick={() => location.reload()}>
          <RotateCwIcon data-icon="inline-start" />
          Reload
        </Button>
      </EmptyContent>
    </Empty>
  )
}
