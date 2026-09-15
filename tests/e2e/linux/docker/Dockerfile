FROM rust:1.95-bookworm AS builder

WORKDIR /src
COPY . .
RUN cargo build --release --locked

FROM debian:bookworm-slim

COPY --from=builder /src/target/release/loadout /usr/local/bin/loadout
COPY tests/fixtures/config/valid /opt/loadout-fixture
COPY tests/e2e/linux/docker/run-scenario.sh /usr/local/bin/loadout-e2e
RUN chmod 755 /usr/local/bin/loadout /usr/local/bin/loadout-e2e

ENTRYPOINT ["/usr/local/bin/loadout-e2e"]
