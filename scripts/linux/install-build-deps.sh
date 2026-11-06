#!/bin/sh
# Installs the Debian/Ubuntu packages the app builds against on Linux:
# GPUI's windowing (X11, Wayland, xkbcommon), fonts, Vulkan, ALSA for
# sound (cpal) and udev for gamepads (gilrs). Used by CI; run it once on a
# new machine.
set -eu

sudo=""
if [ "$(id -u)" != 0 ]; then
    sudo="sudo"
fi

# Only Ubuntu's own packages are needed. GitHub's images list them in
# ubuntu.sources, beside other repositories that can be out of sync;
# Namespace's images have no such file, so read everything they list.
if [ -f /etc/apt/sources.list.d/ubuntu.sources ]; then
    $sudo apt-get update -qq \
        -o Dir::Etc::sourcelist="sources.list.d/ubuntu.sources" \
        -o Dir::Etc::sourceparts="-"
else
    $sudo apt-get update -qq
fi

$sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
    build-essential pkg-config cmake curl unzip \
    libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
    libx11-dev libxcb1-dev libx11-xcb-dev \
    libfontconfig1-dev libfreetype-dev \
    libvulkan-dev libasound2-dev libudev-dev
