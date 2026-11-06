# Ubuntu 24.04 with the packages the app builds and runs against, for
# scripts/linux/cross.sh: its sysroot for cross-compiling from a Mac, and
# the container its tests run in. Xvfb and Mesa's software Vulkan let the
# app itself start without a display or GPU, and the screenshot tools let
# scripts/linux/smoke.sh check what it draws.
FROM ubuntu:24.04
COPY install-build-deps.sh /tmp/install-build-deps.sh
RUN sh /tmp/install-build-deps.sh --smoke \
 && rm -rf /var/lib/apt/lists/* /tmp/install-build-deps.sh
