# static/app.css is generated from styles/input.css and the class names in templates, JS and Rust.
FROM rust:1-trixie AS css
WORKDIR /build
COPY scripts/build-css.sh scripts/
COPY styles ./styles
COPY templates ./templates
COPY static/app.js ./static/
COPY src ./src
RUN scripts/build-css.sh

FROM rust:1-trixie AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY templates ./templates
# The version shown in the app; the release workflow passes the release tag's version.
ARG HOMESTOCK_VERSION=0.0.0-dev
RUN cargo build --release --locked

FROM debian:trixie-slim
RUN useradd --uid 10001 --no-create-home homestock
WORKDIR /opt/homestock
COPY --from=builder /build/target/release/homestock /usr/local/bin/homestock
COPY static ./static
COPY --from=css /build/static/app.css ./static/app.css
USER 10001:10001
EXPOSE 8443
ENTRYPOINT ["/usr/local/bin/homestock"]
CMD ["--config", "/etc/homestock/config.toml"]
