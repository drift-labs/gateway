FROM rust:1.73.0 as builder
WORKDIR /build
COPY  . .
RUN cargo build --release

FROM gcr.io/distroless/base-debian12
COPY --from=builder /lib/x86_64-linux-gnu/libgcc_s.so.1 /lib/x86_64-linux-gnu/libgcc_s.so.1
COPY --from=builder /build/target/release/drift-gateway /bin/drift-gateway
ENTRYPOINT ["/bin/drift-gateway"]