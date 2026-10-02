# Release image. The static musl binaries are built by the release workflow and staged as
# dist/<arch>/stacksup before this builds. There are no RUN steps, so the multi-platform build
# needs no emulation. docker:cli ships the docker CLI + compose plugin stacksup shells out to.
#
# stacksup drives the HOST's docker daemon, and the compose file it renders uses bind mounts the
# daemon resolves as host paths, so mount the deployment directory at the SAME path inside the
# container:
#
#   docker run --rm -it \
#     -v /var/run/docker.sock:/var/run/docker.sock \
#     -v "$PWD":"$PWD" -w "$PWD" \
#     ghcr.io/stx-labs/stacksup start
FROM docker:28-cli
ARG TARGETARCH
COPY dist/${TARGETARCH}/stacksup /usr/local/bin/stacksup
ENTRYPOINT ["stacksup"]
