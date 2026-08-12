import { FaberIndicator } from "@/components/thread/faber-indicator"
import { Markdown } from "@/components/thread/markdown"
import { AgentMessage, AgentRun, AgentStep } from "@/components/ui/agent-run"
import type { ContentBlock, Turn } from "@/lib/thread/transcript"

function userText(turn: Turn): string {
  return turn.userContent
    .filter((block): block is Extract<ContentBlock, { type: "text" }> => block.type === "text")
    .map((block) => block.text)
    .join("\n\n")
}

/** One user turn plus the agent's run in response, on the session timeline. */
export function TurnView({ turn, isLast = false }: { turn: Turn; isLast?: boolean }) {
  const text = userText(turn)

  return (
    <div className="flex flex-col gap-6">
      {text ? (
        <div className="ml-auto max-w-[85%] rounded-2xl bg-muted px-4 py-2.5 text-[15px] leading-relaxed text-foreground">
          <Markdown text={text} softBreaks />
        </div>
      ) : null}

      {/* Rows are composed by hand rather than passed as `items`, because the
          data-driven path renders a message's text as a plain string — the only
          way to hand it to Markdown is to build the row ourselves. `isLast` is
          still AgentRun's to inject. */}
      {turn.items.length > 0 ? (
        <AgentRun>
          {turn.items.map((item) =>
            item.kind === "message" ? (
              <AgentMessage key={item.id}>
                <Markdown text={item.text} />
              </AgentMessage>
            ) : (
              <AgentStep key={item.id} state={item.state} title={item.name}>
                {item.result}
              </AgentStep>
            ),
          )}
        </AgentRun>
      ) : null}

      {/* The indicator is the tail of the timeline, so only the last turn — the
          one that can still be running — carries it. */}
      {isLast ? <FaberIndicator working={turn.status === "running"} /> : null}

      {turn.status === "error" ? (
        <p className="text-sm text-destructive">{turn.errorMessage ?? "The run failed."}</p>
      ) : null}
    </div>
  )
}
