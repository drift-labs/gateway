FROM rust:1.76.0 as builder

# docker automatically sets this to architecture of the host system
# requires DOCKER_BUILDKIT=1
ARG TARGETARCH

RUN apt-get update && apt-get install -y libgcc1
WORKDIR /build
COPY  . .
RUN cargo build --release
RUN if [ "$TARGETARCH" = "arm64" ]; then \
    cp /lib/aarch64-linux-gnu/libgcc_s.so.1 /build/target/release/; \
    elif [ "$TARGETARCH" = "amd64" ]; then \
    cp /lib/x86_64-linux-gnu/libgcc_s.so.1 /build/target/release/; \
    fi

FROM debian:12
COPY --from=builder /build/target/release/libgcc_s.so.1 /lib/
COPY --from=builder /build/target/release/drift-gateway /bin/drift-gateway
ENTRYPOINT ["/bin/drift-gateway"]
