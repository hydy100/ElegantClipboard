import * as React from "react"
import * as TooltipPrimitive from "@radix-ui/react-tooltip"
import { cn } from "@/lib/utils"

const CLOSE_ALL_TOOLTIPS_EVENT = "elegant-clipboard:close-tooltips"
const TooltipIntent = React.createContext<React.MutableRefObject<boolean> | null>(null)
let keyboardIntent = false

/** Close tooltip popups before native window dragging starts. */
export function closeAllTooltips() {
  if (typeof document !== "undefined") {
    document.dispatchEvent(new Event(CLOSE_ALL_TOOLTIPS_EVENT))
  }
}

type TooltipProviderProps = React.ComponentPropsWithoutRef<typeof TooltipPrimitive.Provider>

const TooltipProvider = ({ children, ...props }: TooltipProviderProps) => {
  React.useEffect(() => {
    let closeFrame = 0

    const closeWhenPointerLeavesTrigger = (event: Event) => {
      const target = event.target
      if (target instanceof Element && target.closest("[data-elegant-tooltip-trigger]")) {
        if (closeFrame) window.cancelAnimationFrame(closeFrame)
        closeFrame = 0
        return
      }
      if (closeFrame) return
      closeFrame = window.requestAnimationFrame(() => {
        closeFrame = 0
        closeAllTooltips()
      })
    }
    const resetIntent = () => {
      keyboardIntent = false
      closeAllTooltips()
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Tab") keyboardIntent = true
    }
    const closeOnWindowBlur = resetIntent

    document.addEventListener("pointerdown", resetIntent, true)
    document.addEventListener("keydown", onKeyDown, true)
    window.addEventListener("focus", resetIntent)
    document.addEventListener("pointermove", closeWhenPointerLeavesTrigger, true)
    document.addEventListener("mousemove", closeWhenPointerLeavesTrigger, true)
    window.addEventListener("blur", closeOnWindowBlur)
    return () => {
      document.removeEventListener("pointerdown", resetIntent, true)
      document.removeEventListener("keydown", onKeyDown, true)
      window.removeEventListener("focus", resetIntent)
      document.removeEventListener("pointermove", closeWhenPointerLeavesTrigger, true)
      document.removeEventListener("mousemove", closeWhenPointerLeavesTrigger, true)
      window.removeEventListener("blur", closeOnWindowBlur)
      if (closeFrame) window.cancelAnimationFrame(closeFrame)
    }
  }, [])

  return <TooltipPrimitive.Provider {...props}>{children}</TooltipPrimitive.Provider>
}

type TooltipProps = React.ComponentPropsWithoutRef<typeof TooltipPrimitive.Root>

const Tooltip = ({
  open: controlledOpen,
  defaultOpen,
  onOpenChange,
  ...props
}: TooltipProps) => {
  const intent = React.useRef(false)
  const isControlled = controlledOpen !== undefined
  const [uncontrolledOpen, setUncontrolledOpen] = React.useState(defaultOpen ?? false)
  const open = isControlled ? controlledOpen : uncontrolledOpen

  React.useEffect(() => {
    const close = () => {
      intent.current = false
      if (!isControlled) setUncontrolledOpen(false)
      onOpenChange?.(false)
    }
    document.addEventListener(CLOSE_ALL_TOOLTIPS_EVENT, close)
    return () => document.removeEventListener(CLOSE_ALL_TOOLTIPS_EVENT, close)
  }, [isControlled, onOpenChange])

  const handleOpenChange = (nextOpen: boolean) => {
    // Closing controlled state alone does not cancel Radix's pending open timer.
    if (nextOpen && !intent.current) return
    if (!isControlled) setUncontrolledOpen(nextOpen)
    onOpenChange?.(nextOpen)
  }

  return (
    <TooltipIntent.Provider value={intent}>
    <TooltipPrimitive.Root
      {...props}
      open={open}
      onOpenChange={handleOpenChange}
    />
    </TooltipIntent.Provider>
  )
}

const TooltipTrigger = React.forwardRef<
  React.ElementRef<typeof TooltipPrimitive.Trigger>,
  React.ComponentPropsWithoutRef<typeof TooltipPrimitive.Trigger>
>((props, ref) => {
  const intent = React.useContext(TooltipIntent)
  return (
  <TooltipPrimitive.Trigger
    {...props}
    ref={ref}
    data-elegant-tooltip-trigger
    onFocus={(event) => {
      props.onFocus?.(event)
      // Restored focus after native dragging/closing Settings is not a hover.
      if (!keyboardIntent) event.preventDefault()
      else if (intent) intent.current = true
    }}
    onPointerMove={(event) => {
      props.onPointerMove?.(event)
      if (event.buttons !== 0 || event.pointerType === "touch") {
        event.preventDefault()
        return
      }
      if (intent) intent.current = true
    }}
    onPointerLeave={(event) => {
      if (intent) intent.current = false
      props.onPointerLeave?.(event)
    }}
  />
  )
})
TooltipTrigger.displayName = TooltipPrimitive.Trigger.displayName

const TooltipContent = React.forwardRef<
  React.ElementRef<typeof TooltipPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof TooltipPrimitive.Content>
>(({ className, sideOffset = 4, ...props }, ref) => (
  <TooltipPrimitive.Portal>
    <TooltipPrimitive.Content
      ref={ref}
      sideOffset={sideOffset}
      className={cn(
        "z-50 overflow-hidden rounded-md bg-primary px-3 py-1.5 text-xs text-primary-foreground animate-in fade-in-0 zoom-in-95 data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95 data-[side=bottom]:slide-in-from-top-2 data-[side=left]:slide-in-from-right-2 data-[side=right]:slide-in-from-left-2 data-[side=top]:slide-in-from-bottom-2",
        className
      )}
      {...props}
    />
  </TooltipPrimitive.Portal>
))
TooltipContent.displayName = TooltipPrimitive.Content.displayName

export { Tooltip, TooltipTrigger, TooltipContent, TooltipProvider }
