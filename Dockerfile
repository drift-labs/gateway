FROM rust:1.84.1 AS builder

RUN apt-get update && apt-get install -y libgcc1 jq
WORKDIR /build
COPY  . .
RUN rustup component add rustfmt
RUN SO_URL=$(curl -s https://api.github.com/repos/drift-labs/drift-ffi-sys/releases/latest | jq -r '.assets[] | select(.name=="libdrift_ffi_sys.so") | .browser_download_url') &&\
  curl -L -o libdrift_ffi_sys.so "$SO_URL" &&\
  cp libdrift_ffi_sys.so /usr/local/lib

# DEV: choose to build drift system libs from source or not
# a) default: use prebuilt lib (faster build time)
RUN CARGO_DRIFT_FFI_PATH="/usr/local/lib" cargo build --release
# b) build libdrift_ffi from source (slower build time)
# RUN rustup install 1.76.0-x86_64-unknown-linux-gnu
# RUN CARGO_DRIFT_FFI_STATIC=1 cargo build --release
# RUN ./target/release/drift-gateway --help

RUN cp /lib/x86_64-linux-gnu/libgcc_s.so.1 /build/target/release/

FROM debian:12
COPY --from=builder /build/target/release/libgcc_s.so.1 /lib/
COPY --from=builder /usr/local/lib/libdrift_ffi_sys.so /lib/
COPY --from=builder /build/target/release/drift-gateway /bin/drift-gateway
RUN apt-get update && apt-get install -y curl && rm -rf /var/cache/apt/archives /var/lib/apt/lists/*
ENTRYPOINT ["/bin/drift-gateway"]
