// The query history and the sessions, polled so the header and pages stay current.

import { api } from "@/lib/api"
import { createStore } from "@/lib/store"
import type { HistoryRecord, SessionSummary } from "@/lib/types"

type HistoryState = {
  /** By query ID, oldest first. */
  records: ReadonlyMap<string, HistoryRecord>
  sequence: number
  /** Changes when the history is cleared or localflake restarts. */
  generation: number | null
}

export const history = createStore<HistoryState>({
  records: new Map(),
  sequence: 0,
  generation: null,
})

type HistoryPage = {
  records: HistoryRecord[]
  sequence: number
  generation: number
}

export async function pollHistory(): Promise<void> {
  const { generation, sequence } = history.get()
  let data: HistoryPage
  try {
    data = await api<HistoryPage>(
      `/history?since=${generation == null ? 0 : sequence}`
    )
  } catch {
    return
  }
  if (generation != null && data.generation !== generation) {
    // Cleared, or a new localflake: start again.
    history.set({ records: new Map(), sequence: 0, generation: null })
    return pollHistory()
  }
  if (!data.records.length && generation === data.generation) {
    if (data.sequence !== sequence) {
      history.set((s) => ({ ...s, sequence: data.sequence }))
    }
    return
  }
  history.set((s) => {
    const records = new Map(s.records)
    for (const record of data.records) records.set(record.queryId, record)
    return { records, sequence: data.sequence, generation: data.generation }
  })
}

export async function clearHistory() {
  await api("/history", { method: "DELETE" })
  history.set({ records: new Map(), sequence: 0, generation: null })
  await pollHistory()
}

export const sessions = createStore<SessionSummary[] | null>(null)

export async function pollSessions() {
  try {
    const data = await api<{ sessions: SessionSummary[] }>("/sessions")
    sessions.set(data.sessions)
  } catch {
    // Shown as stale until localflake answers again.
  }
}

let started = false

export function startPolling() {
  if (started) return
  started = true
  void pollHistory()
  void pollSessions()
  setInterval(() => void pollHistory(), 1500)
  setInterval(() => void pollSessions(), 4000)
}
