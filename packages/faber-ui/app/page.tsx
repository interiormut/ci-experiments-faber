import { AmbientBackground } from "@/components/ui/ambient-background";
import { AppSidebar } from "@/components/ui/app-sidebar";
import { PromptBox } from "@/components/ui/prompt-box";

export default function Home() {
  return (
    <>
      <AmbientBackground />
      <div className="relative flex flex-1">
        <AppSidebar />
        <main className="flex min-w-0 flex-1 items-center justify-center p-4">
          <PromptBox
            className="w-full max-w-2xl"
            placeholder="Start a thread…"
          />
        </main>
      </div>
    </>
  );
}
