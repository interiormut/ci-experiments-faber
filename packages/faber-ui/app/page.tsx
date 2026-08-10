import { AppSidebar } from "@/components/shell/app-sidebar";
import { PromptBox } from "@/components/thread/prompt-box";

export default function Home() {
  return (
    <div className="relative flex flex-1">
      <AppSidebar />
      <main className="flex min-w-0 flex-1 items-center justify-center p-4">
        <PromptBox className="w-full max-w-2xl" placeholder="Start a thread…" />
      </main>
    </div>
  );
}
