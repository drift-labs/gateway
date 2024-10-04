FROM rust:1.81.0 AS builder

RUN apt-get update && apt-get install -y libgcc1
WORKDIR /build
COPY  . .
RUN rustup component add rustfmt && rustup install 1.76.0-x86_64-unknown-linux-gnu
RUN curl -L https://github.com/user-attachments/files/17126152/libdrift_ffi_sys.so.zip > ffi.zip && unzip ffi.zip && cp libdrift_ffi_sys.so /usr/local/lib

# DEV: choose to build drift system libs from source or not
# a) default: use prebuilt lib (faster build time)
RUN CARGO_DRIFT_FFI_PATH="/usr/local/lib" cargo build --release
# b) build libdrift_ffi from source (slower build time)
# RUN CARGO_DRIFT_FFI_STATIC=1 cargo build --release
# RUN ./target/release/drift-gateway --help

RUN cp /lib/x86_64-linux-gnu/libgcc_s.so.1 /build/target/release/

FROM debian:12
COPY --from=builder /build/target/release/libgcc_s.so.1 /lib/
COPY --from=builder /usr/local/lib/libdrift_ffi_sys.so /lib/
COPY --from=builder /build/target/release/drift-gateway /bin/drift-gateway
RUN apt-get update && apt-get install -y curl && rm -rf /var/cache/apt/archives /var/lib/apt/lists/*
ENTRYPOINT ["/bin/drift-gateway"]
