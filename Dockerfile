FROM rust:1.81.0 AS builder

# docker automatically sets this to architecture of the host system
# requires DOCKER_BUILDKIT=1
ARG TARGETARCH

RUN apt-get update && apt-get install -y libgcc1
WORKDIR /build
COPY  . .
RUN rustup component add rustfmt
RUN curl -L https://github.com/user-attachments/files/17126152/libdrift_ffi_sys.so.zip > ffi.zip && unzip ffi.zip && cp libdrift_ffi_sys.so /usr/local/lib
RUN CARGO_DRIFT_FFI_PATH="/usr/local/lib" cargo build --release
RUN if [ "$TARGETARCH" = "arm64" ]; then \
    cp /lib/aarch64-linux-gnu/libgcc_s.so.1 /build/target/release/; \
    elif [ "$TARGETARCH" = "amd64" ]; then \
    cp /lib/x86_64-linux-gnu/libgcc_s.so.1 /build/target/release/; \
    fi

FROM gcr.io/distroless/base-debian12
COPY --from=builder /build/target/release/libgcc_s.so.1 /lib/
COPY --from=builder /usr/local/lib/libdrift_ffi_sys.so /lib/
COPY --from=builder /build/target/release/drift-gateway /bin/drift-gateway
ENTRYPOINT ["/bin/drift-gateway"]
