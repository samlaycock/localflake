import { CopyButton } from "@/components/copy-button"
import { highlight } from "@/lib/sql"
import { cn } from "@/lib/utils"

/** SQL, highlighted, with a copy button and an optional label. */
export function SqlCode({
  sql,
  label,
  className,
}: {
  sql: string
  label?: string | null
  className?: string
}) {
  return (
    <div
      className={cn(
        "group/sql relative min-w-0 overflow-hidden rounded-lg border bg-muted/30",
        className
      )}
    >
      {label && (
        <div className="border-b px-3 py-1 text-xs font-medium text-muted-foreground">
          {label}
        </div>
      )}
      <pre
        className="max-h-[60vh] overflow-auto p-3 pr-20 font-mono text-xs leading-5"
        dangerouslySetInnerHTML={{ __html: highlight(sql) }}
      />
      <CopyButton
        text={sql}
        variant="ghost"
        className={cn("absolute right-1.5", label ? "top-8" : "top-1.5")}
      />
    </div>
  )
}
