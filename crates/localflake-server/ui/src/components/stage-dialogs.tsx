import * as React from "react"
import { toast } from "sonner"

import { FileDrop } from "@/components/file-drop"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import {
  Field,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Textarea } from "@/components/ui/textarea"
import { message } from "@/lib/api"
import { refreshCatalogSoon } from "@/lib/catalog"
import { preview, uploadFile } from "@/lib/explorer"
import { qualified } from "@/lib/sql"
import type { CatalogObject } from "@/lib/types"
import { activeId, runSql } from "@/lib/worksheets"

/** Uploads a file to a stage. */
export function UploadDialog({
  stage,
  onClose,
}: {
  stage: string
  onClose: () => void
}) {
  const [file, setFile] = React.useState<File | null>(null)
  const [path, setPath] = React.useState("")
  const [busy, setBusy] = React.useState(false)

  async function upload() {
    if (!file) return
    const dir = path.trim().replace(/^\/+|\/+$/g, "")
    setBusy(true)
    try {
      await uploadFile(dir ? `${stage}/${dir}` : stage, file)
    } catch (e) {
      toast.error(message(e))
      return
    } finally {
      setBusy(false)
    }
    onClose()
    toast(`Uploaded ${file.name}`)
    void preview(`LIST ${stage}`, true)
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="font-mono">Upload to {stage}</DialogTitle>
          <DialogDescription>
            The file is stored as-is (not compressed), like PUT … AUTO_COMPRESS
            = FALSE OVERWRITE = TRUE.
          </DialogDescription>
        </DialogHeader>
        <FieldGroup>
          <FileDrop file={file} onFile={setFile} />
          <Field>
            <FieldLabel htmlFor="upload-dir">Directory in the stage</FieldLabel>
            <Input
              id="upload-dir"
              className="font-mono"
              placeholder="optional/sub/directory"
              spellCheck={false}
              value={path}
              onChange={(e) => setPath(e.target.value)}
            />
          </Field>
        </FieldGroup>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button disabled={!file || busy} onClick={() => void upload()}>
            Upload
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

const FORMATS = ["CSV", "JSON", "PARQUET"].map((f) => ({ label: f, value: f }))
const ON_ERROR = [
  { label: "Abort on the first error", value: "ABORT_STATEMENT" },
  { label: "Skip bad rows", value: "CONTINUE" },
]

/** Loads a CSV, JSON or Parquet file into a table through the user stage. */
export function LoadDataDialog({
  database,
  schema,
  table,
  onClose,
}: {
  database: string
  schema: string
  table: CatalogObject
  onClose: () => void
}) {
  const name = qualified(database, schema, table.name)
  const [file, setFile] = React.useState<File | null>(null)
  const [format, setFormat] = React.useState("CSV")
  const [header, setHeader] = React.useState(true)
  const [delimiter, setDelimiter] = React.useState(",")
  const [onError, setOnError] = React.useState("ABORT_STATEMENT")
  // The COPY statement can be edited; changing an option writes it again.
  const [edited, setEdited] = React.useState<string | null>(null)
  const [busy, setBusy] = React.useState(false)

  const singleVariant =
    table.columns.length === 1 &&
    ["VARIANT", "OBJECT", "ARRAY"].includes(table.columns[0].type)
  const generated = (() => {
    const fileName = file ? file.name.replace(/'/g, "") : "<file>"
    const options: string[] = []
    if (format === "CSV") {
      const delim =
        delimiter === "\\t" || delimiter === "\t"
          ? "\\t"
          : delimiter.replace(/'/g, "''") || ","
      const formatOptions = [`TYPE = CSV`, `FIELD_OPTIONALLY_ENCLOSED_BY = '"'`]
      if (delim !== ",") formatOptions.push(`FIELD_DELIMITER = '${delim}'`)
      formatOptions.push(header ? "PARSE_HEADER = TRUE" : "SKIP_HEADER = 0")
      options.push(`FILE_FORMAT = (${formatOptions.join(" ")})`)
      if (header) options.push("MATCH_BY_COLUMN_NAME = CASE_INSENSITIVE")
    } else {
      options.push(`FILE_FORMAT = (TYPE = ${format})`)
      if (!singleVariant)
        options.push("MATCH_BY_COLUMN_NAME = CASE_INSENSITIVE")
    }
    options.push(`ON_ERROR = ${onError}`, "FORCE = TRUE")
    return `COPY INTO ${name}\nFROM @~/localflake-uploads\nFILES = ('${fileName}')\n${options.join("\n")};`
  })()
  const sql = edited ?? generated

  function chooseFile(chosen: File) {
    setFile(chosen)
    const lower = chosen.name.toLowerCase().replace(/\.(gz|gzip)$/, "")
    if (/\.(json|ndjson|jsonl)$/.test(lower)) setFormat("JSON")
    else if (lower.endsWith(".parquet")) setFormat("PARQUET")
    else {
      setFormat("CSV")
      if (lower.endsWith(".tsv")) setDelimiter("\\t")
    }
    setEdited(null)
  }

  async function load() {
    if (!file) return
    setBusy(true)
    try {
      await uploadFile("@~/localflake-uploads", file)
    } catch (e) {
      toast.error(message(e))
      return
    } finally {
      setBusy(false)
    }
    onClose()
    const data = await runSql(activeId(), sql, {
      label: `Loading ${file.name} into ${name}…`,
    })
    if (data && !data.error) toast(`Loaded ${file.name}`)
    refreshCatalogSoon()
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>
            Load data into <span className="font-mono">{table.name}</span>
          </DialogTitle>
          <DialogDescription>
            The file is uploaded to your user stage (@~/localflake-uploads),
            then the COPY statement loads it.
          </DialogDescription>
        </DialogHeader>
        <FieldGroup>
          <FileDrop
            file={file}
            onFile={chooseFile}
            accept=".csv,.tsv,.txt,.json,.ndjson,.jsonl,.parquet,.gz"
          />
          <div className="grid gap-4 sm:grid-cols-2">
            <Field>
              <FieldLabel>Format</FieldLabel>
              <Select
                items={FORMATS}
                value={format}
                onValueChange={(v) => {
                  if (!v) return
                  setFormat(v)
                  setEdited(null)
                }}
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {FORMATS.map((f) => (
                    <SelectItem key={f.value} value={f.value}>
                      {f.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <Field>
              <FieldLabel>Errors</FieldLabel>
              <Select
                items={ON_ERROR}
                value={onError}
                onValueChange={(v) => {
                  if (!v) return
                  setOnError(v)
                  setEdited(null)
                }}
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {ON_ERROR.map((o) => (
                    <SelectItem key={o.value} value={o.value}>
                      {o.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
          </div>
          {format === "CSV" && (
            <div className="grid items-end gap-4 sm:grid-cols-[1fr_auto]">
              <Field orientation="horizontal">
                <Checkbox
                  id="load-header"
                  checked={header}
                  onCheckedChange={(checked) => {
                    setHeader(checked)
                    setEdited(null)
                  }}
                />
                <FieldLabel htmlFor="load-header" className="font-normal">
                  First row is a header (match columns by name)
                </FieldLabel>
              </Field>
              <Field>
                <FieldLabel htmlFor="load-delimiter">Delimiter</FieldLabel>
                <Input
                  id="load-delimiter"
                  className="w-20 font-mono"
                  value={delimiter}
                  onChange={(e) => {
                    setDelimiter(e.target.value)
                    setEdited(null)
                  }}
                />
              </Field>
            </div>
          )}
          <Field>
            <FieldLabel htmlFor="load-sql">COPY statement</FieldLabel>
            <Textarea
              id="load-sql"
              rows={7}
              spellCheck={false}
              className="font-mono text-xs"
              value={sql}
              onChange={(e) => setEdited(e.target.value)}
            />
            <FieldDescription>Edit it to change any option.</FieldDescription>
          </Field>
        </FieldGroup>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button disabled={!file || busy} onClick={() => void load()}>
            Upload and load
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
