import type { LucideIcon } from "lucide-react"

import {
  ContextMenuContent,
  ContextMenuGroup,
  ContextMenuItem,
  ContextMenuLabel,
  ContextMenuSeparator,
} from "@/components/ui/context-menu"
import {
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
} from "@/components/ui/dropdown-menu"

/** A menu entry; null separates groups, and false leaves an entry out. */
export type MenuItem =
  | {
      label: string
      icon: LucideIcon
      action: () => unknown
      danger?: boolean
      disabled?: boolean
    }
  | null
  | false

/** Splits items into groups at the separators, dropping empty groups. */
function groups(items: MenuItem[]) {
  const result: Exclude<MenuItem, null | false>[][] = [[]]
  for (const item of items) {
    if (item === false) continue
    if (item === null) result.push([])
    else result[result.length - 1].push(item)
  }
  return result.filter((g) => g.length)
}

/** The same items as a dropdown (from a "more" button) or a context menu (right-click). */
export function MenuItems({
  items,
  title,
  as,
  className,
}: {
  items: MenuItem[]
  title?: string
  as: "dropdown" | "context"
  className?: string
}) {
  const [Content, Group, Item, Label, Separator] =
    as === "dropdown"
      ? ([
          DropdownMenuContent,
          DropdownMenuGroup,
          DropdownMenuItem,
          DropdownMenuLabel,
          DropdownMenuSeparator,
        ] as const)
      : ([
          ContextMenuContent,
          ContextMenuGroup,
          ContextMenuItem,
          ContextMenuLabel,
          ContextMenuSeparator,
        ] as const)
  return (
    <Content className={className ?? "w-56"}>
      {groups(items).map((group, g) => (
        <Group key={g}>
          {g > 0 && <Separator />}
          {g === 0 && title && (
            <Label className="truncate font-mono">{title}</Label>
          )}
          {group.map(({ label, icon: Icon, action, danger, disabled }) => (
            <Item
              key={label}
              variant={danger ? "destructive" : "default"}
              disabled={disabled}
              onClick={() => void action()}
            >
              <Icon />
              {label}
            </Item>
          ))}
        </Group>
      ))}
    </Content>
  )
}
