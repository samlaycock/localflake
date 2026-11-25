import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Kbd, KbdGroup } from "@/components/ui/kbd"
import { MOD } from "@/lib/format"

const SHORTCUTS: [string[], string][] = [
  [[MOD, "Enter"], "Run the statement at the cursor, or the selection"],
  [[MOD, "Shift", "Enter"], "Run every statement in the worksheet"],
  [[MOD, "S"], "Save the worksheet as a saved query"],
  [[MOD, "/"], "Comment or uncomment the selected lines"],
  [
    ["Ctrl", "Space"],
    "Complete names: tables, columns (also after “alias.”), keywords",
  ],
  [["Tab"], "Indent (Shift+Tab outdents)"],
  [
    ["Double-click"],
    "In the explorer: insert a name. In results: view a value",
  ],
  [[MOD, "C"], "In results: copy the selected value"],
  [["?"], "Show these shortcuts"],
]

export function ShortcutsDialog({
  open,
  onOpenChange,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Keyboard shortcuts</DialogTitle>
          <DialogDescription className="sr-only">
            The worksheet editor's and results' keyboard shortcuts
          </DialogDescription>
        </DialogHeader>
        <dl className="grid grid-cols-[auto_1fr] items-center gap-x-4 gap-y-2.5 text-sm">
          {SHORTCUTS.map(([keys, text]) => (
            <div key={text} className="contents">
              <dt>
                <KbdGroup>
                  {keys.map((key) => (
                    <Kbd key={key}>{key}</Kbd>
                  ))}
                </KbdGroup>
              </dt>
              <dd className="text-muted-foreground">{text}</dd>
            </div>
          ))}
        </dl>
        <DialogFooter>
          <Button onClick={() => onOpenChange(false)}>Close</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
