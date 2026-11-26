# Agent sandbox image: Node runtime + the ferrobot runner.
# Build from the repository root:  docker build -f runner/Dockerfile -t ferrobot-runner:dev .

FROM node:24-bookworm-slim AS build
WORKDIR /src
COPY runner/package.json runner/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY runner/tsconfig.json runner/tsconfig.build.json ./
COPY runner/src ./src
RUN npm run build

# Runtime dependencies in their own stage: a release that changes only the runner's code
# reuses this layer, so an update downloads just the code.
FROM node:24-bookworm-slim AS deps
WORKDIR /src
COPY runner/package.json runner/package-lock.json ./
RUN npm ci --omit=dev --no-audit --no-fund

FROM node:24-bookworm-slim
# A few everyday tools so the agent's computer is actually usable, and
# Chromium for `ferrobot-pdf` (HTML to PDF).
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git curl ripgrep \
       chromium fonts-dejavu-core fonts-noto-color-emoji \
    && rm -rf /var/lib/apt/lists/*
COPY runner/bin/ferrobot-pdf /usr/local/bin/ferrobot-pdf
# Harnesses for the `codex` and `acp` drivers; they reach the model only through
# the daemon's proxy and never hold a real key.
RUN npm install -g --no-audit --no-fund @openai/codex@0.166.0 && npm cache clean --force
RUN npm install -g --no-audit --no-fund @google/gemini-cli@0.67.0 && npm cache clean --force
WORKDIR /opt/ferrobot-runner
COPY --from=deps /src/package.json ./
COPY --from=deps /src/node_modules ./node_modules
COPY --from=build /src/dist ./dist
ENV NODE_ENV=production
# Non-root; $HOME (/home/node) is the per-agent persistent volume.
USER node
WORKDIR /home/node
CMD ["node", "/opt/ferrobot-runner/dist/index.js"]
