// Confirmations and name prompts that actions anywhere can await, shown by <DialogHost />.

import type * as React from "react"

import { createStore } from "@/lib/store"

export type ConfirmRequest = {
  kind: "confirm"
  title: string
  message?: React.ReactNode
  /** SQL the action runs, shown before it does. */
  sql?: string
  confirmLabel?: string
  danger?: boolean
  resolve: (ok: boolean) => void
}

export type PromptRequest = {
  kind: "prompt"
  title: string
  label: string
  value?: string
  placeholder?: string
  help?: React.ReactNode
  confirmLabel?: string
  resolve: (value: string | null) => void
}

export const dialogRequest = createStore<ConfirmRequest | PromptRequest | null>(
  null
)

function show<T>(
  request: Omit<ConfirmRequest | PromptRequest, "resolve">,
  cancelled: T
) {
  // A new request cancels one still open.
  dialogRequest.get()?.resolve(cancelled as never)
  return new Promise<T>((resolve) => {
    dialogRequest.set({
      ...request,
      resolve: (value: T) => {
        dialogRequest.set(null)
        resolve(value)
      },
    } as ConfirmRequest | PromptRequest)
  })
}

export const confirm = (request: Omit<ConfirmRequest, "kind" | "resolve">) =>
  show<boolean>({ kind: "confirm", ...request }, false)

/** Resolves with the trimmed text, or null when cancelled or empty. */
export const prompt = (request: Omit<PromptRequest, "kind" | "resolve">) =>
  show<string | null>({ kind: "prompt", ...request }, null)
