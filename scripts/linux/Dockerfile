# Ubuntu 24.04 with the packages the app builds and runs against, for
# scripts/linux/cross.sh: its sysroot for cross-compiling from a Mac, and
# the container its tests run in. Xvfb and Mesa's software Vulkan let the
# app itself start without a display or GPU.
FROM ubuntu:24.04
COPY install-build-deps.sh /tmp/install-build-deps.sh
RUN sh /tmp/install-build-deps.sh \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
    ca-certificates xvfb xauth mesa-vulkan-drivers libvulkan1 \
 && rm -rf /var/lib/apt/lists/* /tmp/install-build-deps.sh
