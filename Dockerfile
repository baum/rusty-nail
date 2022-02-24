#FROM gcr.io/distroless/static:nonroot
#FROM ubuntu
FROM debian:11.2-slim
COPY --chown=nonroot:nonroot ./target/debug/controller /app/controller
EXPOSE 8080
#ENTRYPOINT [/app/controller]
