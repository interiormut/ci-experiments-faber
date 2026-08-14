declare const Bun: {
  file(path: string): {
    exists(): Promise<boolean>
    text(): Promise<string>
  } & Blob
  serve(options: {
    port: number
    hostname: string
    fetch(request: Request): Response | Promise<Response>
  }): void
}

const port = Number(process.env.PORT ?? 3000)
const hostname = process.env.HOST ?? "0.0.0.0"

function runtimeScript(): string {
  const apiUrl = (process.env.FABER_API_URL ?? process.env.API_URL ?? "").replace(/\/+$/, "")
  const authMode = process.env.FABER_AUTH_MODE ?? process.env.AUTH_MODE
  const config = JSON.stringify({ apiUrl, authMode }).replace(/</g, "\\u003c")
  return `<script>window.__FABER_RUNTIME_CONFIG__=${config}</script>`
}

async function serve(pathname: string): Promise<Response> {
  const path = pathname === "/" ? "/index.html" : pathname
  const candidates = [
    path,
    path.endsWith("/") ? `${path}index.html` : `${path}/index.html`,
    "/index.html",
  ]

  for (const candidate of candidates) {
    const file = Bun.file(`./out${candidate}`)
    if (!(await file.exists())) continue

    if (!candidate.endsWith(".html")) return new Response(file)

    const body = await file.text()
    return new Response(body.replace("</head>", `${runtimeScript()}</head>`), {
      headers: { "content-type": "text/html; charset=utf-8" },
    })
  }

  return new Response("Not found", { status: 404 })
}

Bun.serve({
  port,
  hostname,
  fetch(request) {
    return serve(new URL(request.url).pathname)
  },
})
