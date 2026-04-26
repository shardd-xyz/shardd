# syntax=docker/dockerfile:1.7
#
# Two-stage build for the shardd.xyz landing site.
#
# Stage 1 (node:20) — installs npm deps and runs vitepress build to
# produce docs/.vitepress/dist/. Asserts via grep that the pre-rendered
# HTML actually contains the hero copy + feature card titles, so an SSR
# regression that empties dist/index.html fails the build instead of
# silently shipping a Vue mount-point.
#
# Stage 2 (caddy:2-alpine) — copies the dist into /srv and runs the
# inner Caddyfile that listens on :8080. The outer Caddy on the
# dashboard host reverse_proxies to this container over the docker
# network; TLS terminates at the outer layer.
#
# Build context is apps/landing/ (NOT the repo root) so that .git is
# never expected in the build context. The commit SHA is injected via
# the GIT_SHA build-arg, surfaced as $GITHUB_SHA inside the build (the
# vitepress config already prefers $GITHUB_SHA over `git rev-parse`).

FROM node:20-alpine AS build
WORKDIR /app

ARG GIT_SHA=unknown
ENV GITHUB_SHA=${GIT_SHA}

COPY package.json package-lock.json ./
RUN npm ci --no-audit --no-fund

COPY docs ./docs
RUN npm run docs:build && \
    grep -q "Balances<br>" docs/.vitepress/dist/index.html && \
    grep -q "at the edge\." docs/.vitepress/dist/index.html && \
    grep -q "Credit, debit, hold" docs/.vitepress/dist/index.html && \
    grep -q "Write from any region" docs/.vitepress/dist/index.html && \
    grep -q "No leader, no consensus" docs/.vitepress/dist/index.html && \
    grep -q "Built for metered billing" docs/.vitepress/dist/index.html && \
    grep -q "POST /events" docs/.vitepress/dist/index.html

FROM caddy:2-alpine
COPY Caddyfile /etc/caddy/Caddyfile
COPY --from=build /app/docs/.vitepress/dist /srv
EXPOSE 8080
