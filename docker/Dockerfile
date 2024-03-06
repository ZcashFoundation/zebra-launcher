# syntax=docker/dockerfile:1
# ===================== Create base stage =====================
ARG NODE_VERSION=lts
ARG WORK_DIR=/usr/src/app
ARG PORT=8080
FROM node:${NODE_VERSION}-slim AS base

ARG WORK_DIR
ARG APP_ENV=production

ARG PORT=${PORT}
ENV WORK_DIR=${WORK_DIR}
ENV NODE_ENV=${APP_ENV}

# Install corepack and set pnpm as default package manager
ENV PNPM_HOME="/pnpm"
ENV PATH="$PNPM_HOME:$PATH"
RUN corepack enable

WORKDIR ${WORK_DIR}

# ===================== Install Deps =====================
FROM base AS deps

COPY package.json pnpm-lock.yaml ./
# By caching the content-addressable store we stop downloading the same packages again and again
RUN --mount=type=cache,id=pnpm,target=/pnpm/store pnpm install --prod --frozen-lockfile

# ===================== Build Stage =====================
# Rebuild the source code only when needed
FROM base AS build

COPY --from=deps ${WORK_DIR}/node_modules ./node_modules
COPY . ./

RUN --mount=type=cache,id=pnpm,target=/pnpm/store pnpm install --frozen-lockfile \
    && pnpm run build

# ===================== App Runner Stage =====================
FROM nginx:stable-alpine as runner

ARG WORK_DIR=/usr/src/app
ARG PORT=8080

WORKDIR ${WORK_DIR}

# use a custom template for nginx
COPY docker/nginx.conf /etc/nginx/conf.d/default.conf.template
COPY --from=deps ${WORK_DIR}/node_modules ./node_modules
COPY --from=build ${WORK_DIR}/dist /usr/share/nginx/html

EXPOSE ${PORT}

ENV PORT ${PORT}
ENV HOST 0.0.0.0

HEALTHCHECK NONE

CMD sh -c "envsubst '\$PORT' < /etc/nginx/conf.d/default.conf.template > /etc/nginx/conf.d/default.conf && nginx -g 'daemon off;'"
