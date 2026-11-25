// Per-browser state: worksheets, saved queries and layout. Storage can be blocked, so every read
// and write tolerates failure. Values are JSON, as the previous UI stored them.

const PREFIX = "localflake."

export function load<T>(key: string, fallback: T): T {
  try {
    const value = localStorage.getItem(PREFIX + key)
    return value == null ? fallback : (JSON.parse(value) as T)
  } catch {
    return fallback
  }
}

export function save(key: string, value: unknown) {
  try {
    localStorage.setItem(PREFIX + key, JSON.stringify(value))
  } catch {
    // Not kept; the page still works.
  }
}

/** Storage for resizable panel layouts that never throws. */
export const layoutStorage = {
  getItem(key: string) {
    try {
      return localStorage.getItem(PREFIX + key)
    } catch {
      return null
    }
  },
  setItem(key: string, value: string) {
    try {
      localStorage.setItem(PREFIX + key, value)
    } catch {
      // Not kept.
    }
  },
}

/** This browser tab's storage: kept across reloads, but not shared with other tabs. */
export function loadSession<T>(key: string, fallback: T): T {
  try {
    const value = sessionStorage.getItem(PREFIX + key)
    return value == null ? fallback : (JSON.parse(value) as T)
  } catch {
    return fallback
  }
}

export function saveSession(key: string, value: unknown) {
  try {
    sessionStorage.setItem(PREFIX + key, JSON.stringify(value))
  } catch {
    // Not kept.
  }
}
