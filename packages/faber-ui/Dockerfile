FROM oven/bun:1 AS builder

WORKDIR /app

COPY packages/faber-ui/package.json packages/faber-ui/bun.lock ./
RUN bun install --frozen-lockfile

COPY packages/faber-ui/ ./

ENV NODE_ENV=production

RUN bun run build

FROM oven/bun:1-slim AS runner

WORKDIR /app

ENV NODE_ENV=production
ENV PORT=3000
ENV HOST=0.0.0.0

COPY --from=builder --chown=bun:bun /app/node_modules ./node_modules
COPY --from=builder --chown=bun:bun /app/out ./out
COPY --from=builder --chown=bun:bun /app/server.ts ./server.ts
COPY --from=builder --chown=bun:bun /app/package.json ./package.json

USER bun

EXPOSE 3000

CMD ["bun", "server.ts"]
