import * as React from "react"

import { SqlCode } from "@/components/sql-code"
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import {
  dialogRequest,
  type ConfirmRequest,
  type PromptRequest,
} from "@/lib/dialogs"
import { useStore } from "@/lib/store"

/** Shows the confirmations and prompts that `confirm()` and `prompt()` ask for. */
export function DialogHost() {
  const request = useStore(dialogRequest)
  if (request?.kind === "confirm") return <ConfirmDialog request={request} />
  if (request?.kind === "prompt") return <PromptDialog request={request} />
  return null
}

function ConfirmDialog({ request }: { request: ConfirmRequest }) {
  return (
    <AlertDialog open onOpenChange={(open) => !open && request.resolve(false)}>
      <AlertDialogContent className="sm:max-w-lg">
        <AlertDialogHeader>
          <AlertDialogTitle>{request.title}</AlertDialogTitle>
          {request.message && (
            <AlertDialogDescription>{request.message}</AlertDialogDescription>
          )}
        </AlertDialogHeader>
        {request.sql && <SqlCode sql={request.sql} />}
        <AlertDialogFooter>
          <AlertDialogCancel onClick={() => request.resolve(false)}>
            Cancel
          </AlertDialogCancel>
          <AlertDialogAction
            variant={request.danger ? "destructive" : "default"}
            onClick={() => request.resolve(true)}
          >
            {request.confirmLabel ?? "Confirm"}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}

function PromptDialog({ request }: { request: PromptRequest }) {
  const [value, setValue] = React.useState(request.value ?? "")
  const submit = () => request.resolve(value.trim() || null)
  return (
    <Dialog open onOpenChange={(open) => !open && request.resolve(null)}>
      <DialogContent className="sm:max-w-md">
        <form
          className="contents"
          onSubmit={(e) => {
            e.preventDefault()
            submit()
          }}
        >
          <DialogHeader>
            <DialogTitle>{request.title}</DialogTitle>
            <DialogDescription className="sr-only">
              {request.label}
            </DialogDescription>
          </DialogHeader>
          <Field>
            <FieldLabel htmlFor="prompt-value">{request.label}</FieldLabel>
            <Input
              id="prompt-value"
              autoFocus
              spellCheck={false}
              placeholder={request.placeholder}
              value={value}
              onChange={(e) => setValue(e.target.value)}
              onFocus={(e) => e.target.select()}
            />
            {request.help && (
              <FieldDescription>{request.help}</FieldDescription>
            )}
          </Field>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => request.resolve(null)}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={!value.trim()}>
              {request.confirmLabel ?? "OK"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
