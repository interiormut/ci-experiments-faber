import { createFileRoute } from "@tanstack/react-router"

import SessionClient from "./-session-client"

export const Route = createFileRoute("/session/$sessionId")({
  component: SessionRoute,
})

function SessionRoute() {
  const { sessionId } = Route.useParams()
  return <SessionClient sessionId={sessionId} />
}
