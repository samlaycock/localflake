import type { Column, Row } from "@/lib/types"

export const isMac = /Mac|iPhone|iPad/.test(navigator.platform)
/** The modifier key's name for shortcuts: ⌘ on a Mac, Ctrl elsewhere. */
export const MOD = isMac ? "⌘" : "Ctrl"

export const plural = (n: number, word: string, many = `${word}s`) =>
  `${n.toLocaleString()} ${n === 1 ? word : many}`

export const compactNumber = (n: number) =>
  n < 10000
    ? n.toLocaleString()
    : new Intl.NumberFormat("en", { notation: "compact" }).format(n)

export function formatDuration(micros: number | null | undefined) {
  if (micros == null) return ""
  if (micros < 1000) return `${micros} µs`
  if (micros < 1e6) return `${(micros / 1000).toFixed(micros < 1e4 ? 1 : 0)} ms`
  return `${(micros / 1e6).toFixed(2)} s`
}

export function relativeTime(millis: number) {
  const seconds = (Date.now() - millis) / 1000
  if (seconds < 45) return "just now"
  if (seconds < 3600) return `${Math.round(seconds / 60)} min ago`
  if (seconds < 86400) return `${Math.round(seconds / 3600)} h ago`
  if (seconds < 86400 * 7) return `${Math.round(seconds / 86400)} d ago`
  return new Date(millis).toLocaleDateString()
}

const pad = (n: number, width = 2) => String(n).padStart(width, "0")

/** The local time of day with milliseconds, for the query history. */
export function timeOfDay(millis: number) {
  const d = new Date(millis)
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`
}

export function download(filename: string, data: BlobPart, type: string) {
  const url = URL.createObjectURL(new Blob([data], { type }))
  const a = document.createElement("a")
  a.href = url
  a.download = filename
  document.body.append(a)
  a.click()
  a.remove()
  setTimeout(() => URL.revokeObjectURL(url), 1000)
}

export const uid = () => crypto.randomUUID()

export function debounce<A extends unknown[]>(
  fn: (...args: A) => void,
  ms: number
) {
  let timer: ReturnType<typeof setTimeout> | undefined
  return (...args: A) => {
    clearTimeout(timer)
    timer = setTimeout(() => fn(...args), ms)
  }
}

// Values: the JSON wire encoding (what drivers receive) to display text --------------------------

/** Seconds since the epoch with nine fractional digits, possibly negative. */
function parseEpoch(text: string) {
  const negative = text.startsWith("-")
  const [whole, frac = ""] = (negative ? text.slice(1) : text).split(".")
  let total =
    BigInt(whole || "0") * 1000000000n + BigInt(`${frac}000000000`.slice(0, 9))
  if (negative) total = -total
  let seconds = total / 1000000000n
  let nanos = total % 1000000000n
  if (nanos < 0n) {
    nanos += 1000000000n
    seconds -= 1n
  }
  return { seconds: Number(seconds), nanos: Number(nanos) }
}

/** Fractional seconds: at least `min` digits, more if they're significant. */
function fraction(nanos: number, min = 3) {
  let digits = pad(nanos, 9).replace(/0+$/, "")
  while (digits.length < min) digits += "0"
  return digits
}

function utcDate(date: Date) {
  const year = date.getUTCFullYear()
  const y = year < 0 ? `-${pad(-year, 4)}` : pad(year, 4)
  return `${y}-${pad(date.getUTCMonth() + 1)}-${pad(date.getUTCDate())}`
}

const utcTime = (date: Date) =>
  `${pad(date.getUTCHours())}:${pad(date.getUTCMinutes())}:${pad(date.getUTCSeconds())}`

function offsetText(minutes: number) {
  const abs = Math.abs(minutes)
  return `${minutes < 0 ? "-" : "+"}${pad(Math.floor(abs / 60))}${pad(abs % 60)}`
}

const zoneFormats = new Map<string, Intl.DateTimeFormat | null>()

/** The UTC offset of an IANA time zone at an instant, in minutes. */
function zoneOffset(millis: number, zone: string | null) {
  if (!zone || zone === "UTC") return 0
  if (!zoneFormats.has(zone)) {
    let format: Intl.DateTimeFormat | null = null
    try {
      format = new Intl.DateTimeFormat("en-US", {
        timeZone: zone,
        hourCycle: "h23",
        era: "short",
        year: "numeric",
        month: "numeric",
        day: "numeric",
        hour: "numeric",
        minute: "numeric",
        second: "numeric",
      })
    } catch {
      // An unknown zone: show UTC.
    }
    zoneFormats.set(zone, format)
  }
  const format = zoneFormats.get(zone)
  if (!format) return 0
  const parts: Record<string, string> = {}
  for (const part of format.formatToParts(new Date(millis))) {
    parts[part.type] = part.value
  }
  let year = Number(parts.year)
  if ((parts.era ?? "").startsWith("B")) year = 1 - year
  const wall = new Date(0)
  wall.setUTCFullYear(year, Number(parts.month) - 1, Number(parts.day))
  wall.setUTCHours(
    Number(parts.hour) % 24,
    Number(parts.minute),
    Number(parts.second),
    0
  )
  return Math.round((wall.getTime() - Math.floor(millis / 1000) * 1000) / 60000)
}

/** A value as text, the way Snowflake's worksheets show it (null stays null). */
export function formatValue(
  value: string | null,
  column: Column,
  timezone: string | null
): string | null {
  if (value == null) return null
  try {
    switch (column.type) {
      case "boolean":
        return value === "1" ? "TRUE" : value === "0" ? "FALSE" : value
      case "date":
        return utcDate(new Date(Number(value) * 86400000))
      case "time": {
        const { seconds, nanos } = parseEpoch(value)
        const text = utcTime(new Date(seconds * 1000))
        return nanos ? `${text}.${fraction(nanos, 0)}` : text
      }
      case "timestamp_ntz": {
        const { seconds, nanos } = parseEpoch(value)
        const date = new Date(seconds * 1000)
        return `${utcDate(date)} ${utcTime(date)}.${fraction(nanos)}`
      }
      case "timestamp_ltz": {
        const { seconds, nanos } = parseEpoch(value)
        const offset = zoneOffset(seconds * 1000, timezone)
        const date = new Date((seconds + offset * 60) * 1000)
        return `${utcDate(date)} ${utcTime(date)}.${fraction(nanos)} ${offsetText(offset)}`
      }
      case "timestamp_tz": {
        const [epoch, zone] = value.split(" ")
        const { seconds, nanos } = parseEpoch(epoch)
        const offset = Number(zone ?? 1440) - 1440
        const date = new Date((seconds + offset * 60) * 1000)
        return `${utcDate(date)} ${utcTime(date)}.${fraction(nanos)} ${offsetText(offset)}`
      }
      default:
        return value
    }
  } catch {
    return value
  }
}

/** `NUMBER(10,2)`, `VARCHAR(20)`, `TIMESTAMP_LTZ`... for result columns. */
export function typeLabel(column: Column) {
  switch (column.type) {
    case "fixed":
      return column.scale
        ? `NUMBER(${column.precision ?? 38},${column.scale})`
        : "NUMBER"
    case "real":
      return "FLOAT"
    case "text":
      return column.length && column.length !== 16777216
        ? `VARCHAR(${column.length})`
        : "VARCHAR"
    case "vector":
      return `VECTOR(${column.fields?.[0]?.type === "fixed" ? "INT" : "FLOAT"}, ${column.vectorDimension})`
    default:
      return column.type.toUpperCase()
  }
}

export const isNumeric = (column: Column) =>
  column.type === "fixed" || column.type === "real"
export const isJson = (column: Column) =>
  ["variant", "object", "array"].includes(column.type)

const TEMPORAL = [
  "date",
  "time",
  "timestamp_ntz",
  "timestamp_ltz",
  "timestamp_tz",
]

/** Compares wire values of a column for sorting (numbers and instants numerically). */
export function compareValues(
  a: string | null,
  b: string | null,
  column: Column
) {
  if (a == null || b == null) return a == null ? (b == null ? 0 : 1) : -1
  if (isNumeric(column) || TEMPORAL.includes(column.type)) {
    const x = parseFloat(a)
    const y = parseFloat(b)
    if (!Number.isNaN(x) && !Number.isNaN(y) && x !== y) return x - y
  }
  return a < b ? -1 : a > b ? 1 : 0
}

/** A row as an object of typed values, for JSON export. */
export function rowObject(columns: Column[], text: (string | null)[]) {
  const object: Record<string, unknown> = {}
  columns.forEach((column, c) => {
    let value: unknown = text[c]
    if (text[c] != null && isJson(column)) {
      try {
        value = JSON.parse(text[c])
      } catch {
        // Keep the text.
      }
    } else if (
      text[c] != null &&
      isNumeric(column) &&
      Number.isSafeInteger(Number(text[c]))
    ) {
      value = Number(text[c])
    } else if (column.type === "boolean" && text[c] != null) {
      value = text[c] === "TRUE"
    }
    object[column.name] = value
  })
  return object
}

/** Values as CSV (RFC 4180) or TSV. */
export function delimited(
  columns: Column[],
  rows: (string | null)[][],
  separator: "," | "\t"
) {
  const quote = (value: string | null) => {
    if (value == null) return ""
    if (separator === "\t") return value.replace(/[\t\n\r]/g, " ")
    return /[",\n\r]/.test(value) || value.trim() !== value
      ? `"${value.replace(/"/g, '""')}"`
      : value
  }
  return `${[
    columns.map((c) => quote(c.name)),
    ...rows.map((row) => row.map(quote)),
  ]
    .map((r) => r.join(separator))
    .join("\n")}\n`
}

/** The formatted text of each value in a row. */
export const rowText = (columns: Column[], row: Row, timezone: string | null) =>
  row.map((v, c) => formatValue(v, columns[c], timezone))
