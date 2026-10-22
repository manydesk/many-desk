# Linux WebKitGTK probe for the Tauri build (Ubuntu 24.04 = what most users run).
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      curl ca-certificates build-essential pkg-config \
      libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev \
      xvfb xauth dbus-x11 gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-nice gstreamer1.0-pipewire \
    && rm -rf /var/lib/apt/lists/*
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
ENV PATH=/root/.cargo/bin:$PATH
WORKDIR /probe
COPY tauri-probe/Cargo.toml tauri-probe/build.rs tauri-probe/tauri.conf.json ./
COPY tauri-probe/src ./src
COPY tauri-probe/dist ./dist
COPY tauri-probe/icons ./icons
COPY tauri-probe/capabilities ./capabilities
RUN cargo build --release --features custom-protocol
CMD ["bash", "-lc", "dbus-launch --exit-with-session xvfb-run -a -s '-screen 0 1280x800x24' timeout 90 ./target/release/tauri-probe 2>&1 | grep -E 'PROBE_RESULT|rror' ; dpkg -s libwebkit2gtk-4.1-0 | grep Version"]
