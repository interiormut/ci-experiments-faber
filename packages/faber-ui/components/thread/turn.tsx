import { Boxes } from "lucide-react"

import { FaberIndicator } from "@/components/thread/faber-indicator"
import { Markdown } from "@/components/thread/markdown"
import { AgentMessage, AgentRun, AgentStep, AgentThinking } from "@/components/ui/agent-run"
import type { ContentBlock, Turn } from "@/lib/thread/transcript"
import { useThinkingModes } from "@/lib/thread/use-thinking-modes"

function userText(turn: Turn): string {
  return turn.userContent
    .filter((block): block is Extract<ContentBlock, { type: "text" }> => block.type === "text")
    .map((block) => block.text)
    .join("\n\n")
}

/** One user turn plus the agent's run in response, on the session timeline. */
export function TurnView({ turn, isLast = false }: { turn: Turn; isLast?: boolean }) {
  const text = userText(turn)
  const thinking = useThinkingModes()

  return (
    <div className="flex flex-col gap-6">
      {text ? (
        <div className="ml-auto max-w-[85%] rounded-2xl bg-muted px-4 py-2.5 text-[15px] leading-relaxed text-foreground">
          <Markdown text={text} softBreaks />
        </div>
      ) : null}

      {/* Neither side's words: the session saying an environment was added.
          Centered and quiet, so it reads as a change to the conversation
          rather than as something someone said in it. */}
      {turn.notices.map((notice, index) => (
        <p
          key={`${turn.runId}:notice:${index}`}
          className="mx-auto flex items-center gap-1.5 text-xs text-muted-foreground"
        >
          <Boxes className="size-3.5" />
          {notice}
        </p>
      ))}

      {/* Rows are composed by hand rather than passed as `items`, because the
          data-driven path renders a message's text as a plain string — the only
          way to hand it to Markdown is to build the row ourselves. `isLast` is
          still AgentRun's to inject. */}
      {turn.items.length > 0 ? (
        <AgentRun>
          {turn.items.map((item) => {
            if (item.kind === "message") {
              return (
                <AgentMessage key={item.id}>
                  <Markdown text={item.text} />
                </AgentMessage>
              )
            }
            if (item.kind === "thinking") {
              const mode = thinking.modeOf(item.id, item.streaming)
              return (
                // Plain text, not Markdown: `peek` sizes its window in lines of
                // this body's own leading, which a block element's margins
                // would throw off.
                <AgentThinking
                  key={item.id}
                  mode={mode}
                  onModeChange={() => thinking.toggle(item.id, mode)}
                >
                  <p className="whitespace-pre-wrap">{item.text}</p>
                </AgentThinking>
              )
            }
            return (
              <AgentStep key={item.id} state={item.state} title={item.name}>
                {item.result}
              </AgentStep>
            )
          })}
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
