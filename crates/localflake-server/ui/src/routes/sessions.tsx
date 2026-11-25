import { ListIcon, RefreshCwIcon, XIcon } from "lucide-react"
import { Link } from "react-router"
import { toast } from "sonner"

import { StatusBadge } from "@/components/status-badge"
import { Badge } from "@/components/ui/badge"
import { Button, buttonVariants } from "@/components/ui/button"
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@/components/ui/empty"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { api, message } from "@/lib/api"
import { confirm } from "@/lib/dialogs"
import { relativeTime } from "@/lib/format"
import { pollSessions, sessions } from "@/lib/live"
import { useStore } from "@/lib/store"
import type { SessionSummary } from "@/lib/types"
import { forgetSession } from "@/lib/worksheets"

/** Every client connected to localflake. */
export function SessionsView() {
  const list = useStore(sessions)
  return (
    <div className="flex flex-col">
      <div className="flex items-start gap-4 border-b px-4 py-3">
        <p className="flex-1 text-sm text-muted-foreground">
          Every client connected to localflake: drivers, and this page's
          worksheets. Ending a session makes its client's next request fail with
          Snowflake's “session no longer exists” error (390111), which is handy
          for testing reconnects.
        </p>
        <Button variant="outline" size="sm" onClick={() => void pollSessions()}>
          <RefreshCwIcon data-icon="inline-start" />
          Refresh
        </Button>
      </div>
      <Table>
        <TableHeader>
          <TableRow className="hover:bg-transparent">
            <TableHead className="pl-4">Session</TableHead>
            <TableHead>Client</TableHead>
            <TableHead>User</TableHead>
            <TableHead>Context</TableHead>
            <TableHead>Logged in</TableHead>
            <TableHead>Last active</TableHead>
            <TableHead className="text-right">Requests</TableHead>
            <TableHead>Status</TableHead>
            <TableHead />
          </TableRow>
        </TableHeader>
        <TableBody>
          {list?.map((s) => (
            <SessionRow key={s.id} session={s} />
          ))}
        </TableBody>
      </Table>
      {list && !list.length && (
        <Empty>
          <EmptyHeader>
            <EmptyTitle>No sessions</EmptyTitle>
            <EmptyDescription>
              Connect a driver, or run a query in a worksheet.
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      )}
    </div>
  )
}

function SessionRow({ session: s }: { session: SessionSummary }) {
  async function end() {
    const ok = await confirm({
      title: `End session #${s.id}`,
      message: `End ${s.client || "this client"}'s session${s.running ? " and cancel the statement it's running" : ""}? Its next request fails with error 390111.`,
      confirmLabel: "End session",
      danger: true,
    })
    if (!ok) return
    try {
      await api(`/sessions/${s.id}`, { method: "DELETE" })
      forgetSession(s.id)
      await pollSessions()
    } catch (e) {
      toast.error(message(e))
    }
  }

  return (
    <TableRow>
      <TableCell className="pl-4 font-mono text-xs">#{s.id}</TableCell>
      <TableCell>
        {s.webUi && <Badge className="mr-1.5">web UI</Badge>}
        {s.client || <span className="text-muted-foreground">unknown</span>}
      </TableCell>
      <TableCell>{s.user}</TableCell>
      <TableCell className="font-mono text-xs text-muted-foreground">
        {[s.database, s.schema].filter(Boolean).join(".") || "none"}
      </TableCell>
      <TableCell title={new Date(s.loggedInAt).toLocaleString()}>
        {relativeTime(s.loggedInAt)}
      </TableCell>
      <TableCell title={new Date(s.lastActiveAt).toLocaleString()}>
        {relativeTime(s.lastActiveAt)}
      </TableCell>
      <TableCell className="text-right tabular-nums">
        {s.requests.toLocaleString()}
      </TableCell>
      <TableCell>
        <StatusBadge status={s.running ? "running" : "idle"} />
      </TableCell>
      <TableCell className="pr-4 text-right">
        <div className="flex justify-end gap-2">
          <Link
            to={`/history?session=${s.id}`}
            className={buttonVariants({ variant: "outline", size: "sm" })}
          >
            <ListIcon data-icon="inline-start" />
            Queries
          </Link>
          <Button variant="destructive" size="sm" onClick={() => void end()}>
            <XIcon data-icon="inline-start" />
            End
          </Button>
        </div>
      </TableCell>
    </TableRow>
  )
}
