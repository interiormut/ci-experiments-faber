import { AgentRun } from "@/components/ui/agent-run"
import type { ContentBlock, Turn } from "@/lib/thread/transcript"

function userText(turn: Turn): string {
  return turn.userContent
    .filter((block): block is Extract<ContentBlock, { type: "text" }> => block.type === "text")
    .map((block) => block.text)
    .join("\n\n")
}

/** One user turn plus the agent's run in response, on the session timeline. */
export function TurnView({ turn }: { turn: Turn }) {
  const text = userText(turn)

  return (
    <div className="flex flex-col gap-6">
      {text ? (
        <div className="ml-auto max-w-[85%] whitespace-pre-wrap rounded-2xl bg-muted px-4 py-2.5 text-[15px] leading-relaxed text-foreground">
          {text}
        </div>
      ) : null}

      {turn.items.length > 0 ? (
        <AgentRun items={turn.items} />
      ) : turn.status === "running" ? (
        <p className="text-sm text-muted-foreground">Thinking…</p>
      ) : null}

      {turn.status === "error" ? (
        <p className="text-sm text-destructive">{turn.errorMessage ?? "The run failed."}</p>
      ) : null}
    </div>
  )
}
