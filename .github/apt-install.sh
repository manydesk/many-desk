#!/usr/bin/env bash
# Installs Ubuntu packages on a GitHub runner without depending on Canonical's servers.
#   .github/apt-install.sh <package>...
# Canonical's servers (archive, security, ports) send runners in Azure to
# azure.archive.ubuntu.com and hang when that is down, and apt's mirror lists spread
# requests over every entry, so one dead entry stalls the whole update. Instead:
#   - measure independent mirrors (a ~1.5 MB index each) and try the fastest first,
#     since the same mirror can be 20x slower from one runner than from another;
#   - point apt at one mirror, with the image's old lists removed (left in place, they
#     hid packages that the new mirror's lists had);
#   - when the update or the install fails there, start over on the next mirror.
set -uo pipefail
src=/etc/apt/sources.list.d/ubuntu.sources
. /etc/os-release
arch=$(dpkg --print-architecture)
if grep -q ubuntu-ports "$src"; then
  candidates="http://mirrors.ocf.berkeley.edu/ubuntu-ports/ http://mirrors.mit.edu/ubuntu-ports/ http://ftp.tsukuba.wide.ad.jp/Linux/ubuntu-ports/"
else
  candidates="http://mirrors.edge.kernel.org/ubuntu/ http://mirrors.ocf.berkeley.edu/ubuntu/ http://mirrors.mit.edu/ubuntu/ http://mirror.math.princeton.edu/pub/ubuntu/"
fi
ranked=$(for m in $candidates; do
  speed=$(curl -fsS --max-time 15 -o /dev/null -w '%{speed_download}' "${m}dists/${VERSION_CODENAME}/main/binary-${arch}/Packages.gz" 2>/dev/null) || speed=0
  echo "${speed%.*} $m"
done | sort -rn)
echo "mirror speeds (bytes/s):"; echo "$ranked"
printf 'Acquire::Retries "3";\nAcquire::http::Timeout "20";\n' | sudo tee /etc/apt/apt.conf.d/80-retries > /dev/null
while read -r speed m; do
  [ "${speed:-0}" -gt 0 ] || continue
  echo "apt mirror: $m"
  sudo sed -i -E "s#^URIs: .*#URIs: $m#" "$src"
  sudo rm -rf /var/lib/apt/lists/*
  if sudo apt-get update -q -o APT::Update::Error-Mode=any &&
     sudo apt-get install -y -q --no-install-recommends "$@"; then
    exit 0
  fi
  echo "::warning::apt failed with $m, next mirror"
done <<< "$ranked"
echo "::error::no Ubuntu mirror could install: $*"
exit 1
