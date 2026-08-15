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
  const file = Bun.file(`./dist${pathname}`)

  if (pathname !== "/" && pathname !== "/index.html" && (await file.exists())) {
    return new Response(file, {
      headers: pathname.startsWith("/assets/")
        ? { "cache-control": "public, max-age=31536000, immutable" }
        : {},
    })
  }

  // Hashed assets are content-addressed: a miss is a genuine 404, never the
  // shell. Returning HTML here would hand a stale client an opaque MIME error
  // instead of a chunk-load failure it can recover from.
  if (pathname.startsWith("/assets/")) return new Response("Not found", { status: 404 })

  const html = await Bun.file("./dist/index.html").text()
  return new Response(html.replace("</head>", `${runtimeScript()}</head>`), {
    headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" },
  })
}

Bun.serve({ port, hostname, fetch: (request) => serve(new URL(request.url).pathname) })
