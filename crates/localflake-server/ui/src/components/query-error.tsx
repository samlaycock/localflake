import { CircleXIcon } from "lucide-react"
import { Link } from "react-router"

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import type { QueryError } from "@/lib/types"

/** A statement's error: Snowflake's code, SQLSTATE and message. */
export function QueryErrorView({ error }: { error: QueryError }) {
  return (
    <Alert variant="destructive">
      <CircleXIcon />
      <AlertTitle className="flex flex-wrap items-baseline gap-x-4 gap-y-1">
        <span>Error</span>
        {error.code && (
          <span className="font-normal">
            Code <span className="font-mono font-medium">{error.code}</span>
          </span>
        )}
        {error.sqlState && (
          <span className="font-normal">
            SQLSTATE{" "}
            <span className="font-mono font-medium">{error.sqlState}</span>
          </span>
        )}
        {error.queryId && (
          <span className="font-normal">
            Query ID <span className="font-mono">{error.queryId}</span>{" "}
            <Link
              to={`/history/${encodeURIComponent(error.queryId)}`}
              className="underline underline-offset-3"
            >
              details
            </Link>
          </span>
        )}
      </AlertTitle>
      <AlertDescription>
        <pre className="font-mono text-xs leading-5 break-words whitespace-pre-wrap">
          {error.message}
        </pre>
      </AlertDescription>
    </Alert>
  )
}
