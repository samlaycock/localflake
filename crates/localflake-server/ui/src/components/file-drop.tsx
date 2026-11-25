import { UploadIcon } from "lucide-react"
import * as React from "react"

import { cn } from "@/lib/utils"

/** Choose a file, or drop one here. */
export function FileDrop({
  file,
  onFile,
  accept,
}: {
  file: File | null
  onFile: (file: File) => void
  accept?: string
}) {
  const [over, setOver] = React.useState(false)
  return (
    <label
      className={cn(
        "flex cursor-pointer flex-col items-center gap-1 rounded-lg border border-dashed px-4 py-6 text-center text-sm text-muted-foreground transition-colors focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/50 hover:bg-muted/50",
        over && "border-primary bg-primary/5"
      )}
      onDragOver={(e) => {
        e.preventDefault()
        setOver(true)
      }}
      onDragLeave={() => setOver(false)}
      onDrop={(e) => {
        e.preventDefault()
        setOver(false)
        const dropped = e.dataTransfer.files[0]
        if (dropped) onFile(dropped)
      }}
    >
      <UploadIcon className="mb-1 size-5" />
      {file ? (
        <span>
          <span className="font-medium text-foreground">{file.name}</span> ·{" "}
          {(file.size / 1024).toFixed(file.size < 10240 ? 1 : 0)} KB
        </span>
      ) : (
        <span>
          <span className="font-medium text-foreground">Choose a file</span> or
          drop it here
        </span>
      )}
      <input
        type="file"
        accept={accept}
        className="sr-only"
        onChange={(e) => {
          const chosen = e.target.files?.[0]
          if (chosen) onFile(chosen)
        }}
      />
    </label>
  )
}
