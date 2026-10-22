ARG BASE=ubuntu:24.04
FROM ${BASE}
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends python3-gi gir1.2-webkit2-4.1 gir1.2-gtk-3.0 xvfb xauth dbus-x11 \
    gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-libav && rm -rf /var/lib/apt/lists/*
COPY probe.py /probe/probe.py
COPY probe2.html /probe/probe.html
CMD ["bash", "-lc", "dbus-launch --exit-with-session xvfb-run -a -s '-screen 0 1280x800x24' timeout 90 python3 /probe/probe.py 2>&1 | grep -E 'WEBKIT|PROBE_RESULT'"]
