# syntax=docker/dockerfile:1

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
ENV HOSTNAME=0.0.0.0
ENV PORT=3000

COPY --from=builder --chown=bun:bun /app/node_modules ./node_modules
COPY --from=builder --chown=bun:bun /app/.next ./.next
COPY --from=builder --chown=bun:bun /app/package.json /app/next.config.ts /app/tsconfig.json ./

USER bun

EXPOSE 3000

CMD ["bun", "run", "start"]
