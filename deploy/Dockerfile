# Builds the by-human server for a Linux host. The chat page and the
# migrations are compiled into the binary, so the image needs no other files.
FROM rust:1.96-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY server ./server
RUN cargo build --release --locked -p by-human --bin by-human

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
# The ChatGPT login lives in a volume mounted at the auth directory. Creating
# it here gives a new volume the owner and mode the login store requires.
RUN useradd --system --uid 10001 --create-home --home-dir /home/by-human by-human \
    && install -d -m 700 -o by-human -g by-human /home/by-human/.config/by-human
COPY --from=build /src/target/release/by-human /usr/local/bin/by-human
USER by-human
ENV BY_HUMAN_AUTH_DIR=/home/by-human/.config/by-human
ENTRYPOINT ["by-human"]
CMD ["serve"]
