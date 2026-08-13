# Build context is this directory (apps/admin), not the repository root: the
# panel is its own npm project and depends on nothing else in the tree.
FROM node:22-alpine AS build
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci
COPY . .
# Vite inlines VITE_* at build time, so the endpoint is baked into the bundle.
# The default is relative on purpose — nginx proxies it to the engine below, so
# the panel and the engine share an origin whatever port the host publishes.
ARG VITE_DONAT_GRAPHQL_URL=/v1/graphql
ARG VITE_DONAT_ROLE=admin
# The deployments this build serves; see .env.example.
ARG VITE_DONAT_STANDS=
# Where /idp/authorize reaches the identity provider's API. Relative, because
# nginx proxies it to the provider on this same origin — which is what lets a
# browser call it at all. Empty switches that screen off. See README.md.
ARG VITE_DONAT_IDP_BASE=/auth/v1
# Offer "create an account" on the sign-in screen. Whether anyone actually may
# is the provider's decision; this only says whether to show the form.
ARG VITE_DONAT_IDP_REGISTRATION=false
RUN npm run build

FROM nginx:alpine
# nginx:alpine renders /etc/nginx/templates/*.template with envsubst at start,
# so the engine's address stays a runtime setting rather than a rebuild.
COPY nginx.conf.template /etc/nginx/templates/default.conf.template
COPY --from=build /app/dist /usr/share/nginx/html
