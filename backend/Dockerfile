# syntax=docker/dockerfile:1
FROM rust:1-bookworm AS build
WORKDIR /app
# Build dependencies first so they stay cached until Cargo.toml/Cargo.lock change.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src
COPY src ./src
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 app && mkdir /data && chown app /data
COPY --from=build /app/target/release/sehat-saathi /usr/local/bin/sehat-saathi
COPY knowledge /app/knowledge
ENV APP_ENV=production PORT=8080 KNOWLEDGE_DIR=/app/knowledge DATABASE_URL=sqlite:///data/sehat.db?mode=rwc
VOLUME /data
USER app
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s CMD curl -fsS http://localhost:8080/api/health || exit 1
CMD ["sehat-saathi"]
