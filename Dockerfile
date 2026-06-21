FROM ghcr.io/getzola/zola:v0.20.0 AS builder
ARG GIT_COMMIT_SHA=unknown
ENV GIT_COMMIT_SHA=${GIT_COMMIT_SHA}
COPY blog /site
WORKDIR /site
RUN ["zola", "build"]

FROM nginx:alpine
COPY --from=builder /site/public /usr/share/nginx/html
EXPOSE 80
