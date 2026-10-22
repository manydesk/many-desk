# Linux build/test image for the Tauri app (Ubuntu 24.04 = the most common desktop base).
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      curl ca-certificates build-essential pkg-config clang nasm file \
      libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev \
      libxcb1-dev libxcb-randr0-dev libxcb-shm0-dev libxrandr-dev libxdo-dev libdbus-1-dev \
      libpipewire-0.3-dev libwayland-dev libegl-dev libgbm-dev libxtst-dev libxi-dev \
      xvfb xauth dbus-x11 x11-apps imagemagick xdotool \
      gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-libav \
    && rm -rf /var/lib/apt/lists/*
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
ENV PATH=/root/.cargo/bin:$PATH
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - && apt-get install -y nodejs && rm -rf /var/lib/apt/lists/*
WORKDIR /src
# AppImage bundling (linuxdeploy + its gtk/gstreamer plugins)
RUN apt-get update && apt-get install -y --no-install-recommends patchelf libgtk-3-bin libgstreamer1.0-dev gstreamer1.0-tools \
    && rm -rf /var/lib/apt/lists/*
# Native egui app (app/): X11/Wayland windowing + OpenGL (Mesa software GL under Xvfb)
RUN apt-get update && apt-get install -y --no-install-recommends libxkbcommon-dev libxkbcommon-x11-0 libgl1-mesa-dev libegl1-mesa-dev libgl1-mesa-dri mesa-utils \
    && rm -rf /var/lib/apt/lists/*
# Voice: ALSA (cpal) and CMake for the bundled libopus
RUN apt-get update && apt-get install -y --no-install-recommends libasound2-dev cmake \
    && rm -rf /var/lib/apt/lists/*
ENV CMAKE_POLICY_VERSION_MINIMUM=3.5
# Viewer hardware decoding: GStreamer (VA-API decoders come from plugins-bad at run time)
RUN apt-get update && apt-get install -y --no-install-recommends libgstreamer-plugins-base1.0-dev \
    && rm -rf /var/lib/apt/lists/*
