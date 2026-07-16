FROM rust:1.95-alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release && strip target/release/homeostat

FROM scratch
COPY --from=builder /app/target/release/homeostat /homeostat
USER 1000
ENTRYPOINT ["/homeostat"]
