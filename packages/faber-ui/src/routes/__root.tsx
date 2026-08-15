import { createRootRoute, Outlet } from "@tanstack/react-router"

import { AmbientBackground } from "@/components/ui/ambient-background"
import { AppAuth } from "@/components/shell/app-auth"
import { AppShell } from "@/components/shell/app-shell"

export const Route = createRootRoute({
  component: RootLayout,
})

function RootLayout() {
  return (
    <>
      <AmbientBackground />
      <AppAuth>
        <AppShell>
          <Outlet />
        </AppShell>
      </AppAuth>
    </>
  )
}
