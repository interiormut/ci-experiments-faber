"use client"

import * as React from "react"
import { Paperclip, SendHorizontal, Square } from "lucide-react"

import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"
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

const INTERRUPT_CONFIRM_THRESHOLD = 200

export type PromptBoxProps = {
  onSend?: (message: string) => boolean | void | Promise<boolean | void>
  onInterrupt?: () => void | Promise<void>
  isExecuting?: boolean
  streamedChars?: number
  disabled?: boolean
  sendDisabled?: boolean
  placeholder?: string
  className?: string
  /** Extra controls for the footer, shown beside the attach button. */
  footerActions?: React.ReactNode
}

export function PromptBox({
  onSend = () => true,
  onInterrupt,
  isExecuting = false,
  streamedChars = 0,
  disabled = false,
  sendDisabled = false,
  placeholder = "Type a message...",
  className,
  footerActions,
}: PromptBoxProps) {
  const textareaRef = React.useRef<HTMLTextAreaElement>(null)
  const [message, setMessage] = React.useState("")
  const [confirmOpen, setConfirmOpen] = React.useState(false)

  const handleSubmit = async () => {
    if (!message.trim() || disabled || sendDisabled) return

    const accepted = await onSend(message)
    if (accepted === false) {
      return
    }

    setMessage("")
    if (textareaRef.current) {
      textareaRef.current.style.height = "auto"
    }
  }

  const handleInterrupt = () => {
    if (!onInterrupt) return
    if (streamedChars >= INTERRUPT_CONFIRM_THRESHOLD) {
      setConfirmOpen(true)
    } else {
      void onInterrupt()
    }
  }

  const handleConfirmedInterrupt = () => {
    setConfirmOpen(false)
    void onInterrupt?.()
  }

  return (
    <>
      <div className={cn("w-full", className)}>
        <div className="rounded-[1rem] border border-input bg-background/90 backdrop-blur-sm shadow-[0_14px_40px_rgba(0,0,0,0.06)] focus-within:ring-2 focus-within:ring-primary/40 transition-all">
          <textarea
            ref={textareaRef}
            value={message}
            onChange={(e) => setMessage(e.target.value)}
            placeholder={placeholder}
            autoComplete="off"
            disabled={disabled}
            rows={1}
            className="min-h-[56px] max-h-[220px] w-full resize-none overflow-y-auto bg-transparent px-3 py-3 text-[15px] leading-relaxed text-foreground placeholder:text-muted-foreground focus:outline-none disabled:opacity-50"
            onInput={(e) => {
              const textarea = e.currentTarget
              textarea.style.height = "auto"
              textarea.style.height = `${Math.min(textarea.scrollHeight, 220)}px`
            }}
            onKeyDown={(e) => {
              if (e.key === "Escape" && isExecuting) {
                e.preventDefault()
                handleInterrupt()
                return
              }
              if (e.key === "Enter" && !e.shiftKey && !isExecuting) {
                e.preventDefault()
                void handleSubmit()
              }
            }}
          />
          <div className="flex flex-wrap items-end justify-between gap-3 px-2 pb-2">
            <div className="flex min-w-0 flex-1 items-center gap-1">
              <Button size="icon" variant="outline" disabled={disabled}>
                <Paperclip className="size-4" />
              </Button>
              {footerActions}
            </div>
            {isExecuting ? (
              <Button
                size="icon"
                variant="outline"
                disabled={disabled || !onInterrupt}
                onClick={handleInterrupt}
                className="transition-all"
                aria-label="Interrupt"
              >
                <Square className="size-4 fill-current" />
              </Button>
            ) : (
              <Button
                size="icon"
                disabled={disabled || sendDisabled || !message.trim()}
                onClick={() => void handleSubmit()}
                className="transition-all"
              >
                <SendHorizontal className="size-4" />
              </Button>
            )}
          </div>
        </div>
      </div>

      <AlertDialog open={confirmOpen} onOpenChange={setConfirmOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Interrupt agent?</AlertDialogTitle>
            <AlertDialogDescription>
              The agent has been running for a while. Interrupting now may leave
              work incomplete.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Keep running</AlertDialogCancel>
            <AlertDialogAction onClick={handleConfirmedInterrupt}>
              Interrupt
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  )
}
