# syntax=docker/dockerfile:1.7
#
# The administration console.
#
# Two stages: Node builds the static tree, nginx serves it. The runtime carries no Node,
# no package manager and no source — only the built files and a server configuration, so
# the image is small and there is nothing in it to exploit at runtime.
#
# The build is reproducible from the lockfile (`npm ci`), and the image is published by
# `.github/workflows/_web.yml` under a digest, never a floating tag.

FROM node:22-alpine AS build

WORKDIR /app

# `npm ci` rather than `npm install`: it installs exactly the lockfile and fails if the
# lockfile and the manifest disagree, which is the difference between a build and a guess.
COPY web/admin/package.json web/admin/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm ci

COPY web/admin/ ./
# The design system is a repository file, not a copy inside the app: `src/styles.css`
# imports it by relative path, so the build needs it where that path points.
COPY libs/ui-tokens/tokens.css /libs/ui-tokens/tokens.css
COPY docs/ui/styles/04-organic/style.css /docs/ui/styles/04-organic/style.css
RUN npm run build

FROM nginx:1.27-alpine

# The `nginx` user in the official image is uid 101. Declared here so the Deployment's
# `runAsUser` and the image agree rather than coinciding.
COPY --from=build /app/dist /usr/share/nginx/html
COPY web/admin/nginx.conf /etc/nginx/nginx.conf

USER 101:101

EXPOSE 8080

CMD ["nginx", "-g", "daemon off;"]
