import { CheckIcon, CircleIcon, XIcon } from "lucide-react"

import { Badge } from "@/components/ui/badge"
import { Spinner } from "@/components/ui/spinner"

const LABELS = {
  running: "Running",
  succeeded: "Succeeded",
  failed: "Failed",
  idle: "Idle",
}

export function StatusBadge({ status }: { status: keyof typeof LABELS }) {
  switch (status) {
    case "running":
      return (
        <Badge variant="outline" className="text-primary">
          <Spinner data-icon="inline-start" />
          {LABELS.running}
        </Badge>
      )
    case "succeeded":
      return (
        <Badge variant="secondary">
          <CheckIcon data-icon="inline-start" />
          {LABELS.succeeded}
        </Badge>
      )
    case "failed":
      return (
        <Badge variant="destructive">
          <XIcon data-icon="inline-start" />
          {LABELS.failed}
        </Badge>
      )
    default:
      return (
        <Badge variant="outline" className="text-muted-foreground">
          <CircleIcon data-icon="inline-start" />
          {LABELS.idle}
        </Badge>
      )
  }
}
