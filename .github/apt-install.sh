#!/usr/bin/env bash
# Installs Ubuntu packages on a GitHub runner without depending on Canonical's servers.
#   .github/apt-install.sh <package>...
# Canonical's servers (archive, security, ports) send runners in Azure to
# azure.archive.ubuntu.com and hang when that is down, and apt's mirror lists spread
# requests over every entry, so one dead entry stalls the whole update. Instead: take
# independent mirrors in order, point apt at one, and when the update or the install
# fails there (unreachable, mid-sync, a damaged index) start over with the next.
set -uo pipefail
src=/etc/apt/sources.list.d/ubuntu.sources
. /etc/os-release
if grep -q ubuntu-ports "$src"; then
  mirrors="http://mirrors.ocf.berkeley.edu/ubuntu-ports/ http://mirrors.mit.edu/ubuntu-ports/ http://ftp.tsukuba.wide.ad.jp/Linux/ubuntu-ports/"
else
  mirrors="http://mirrors.edge.kernel.org/ubuntu/ http://mirrors.ocf.berkeley.edu/ubuntu/ http://mirrors.mit.edu/ubuntu/"
fi
printf 'Acquire::Retries "3";\nAcquire::http::Timeout "20";\n' | sudo tee /etc/apt/apt.conf.d/80-retries > /dev/null
for m in $mirrors; do
  if ! curl -fsS --max-time 10 -o /dev/null "${m}dists/${VERSION_CODENAME}-security/InRelease"; then
    echo "::warning::apt mirror $m does not answer, next"
    continue
  fi
  echo "apt mirror: $m"
  sudo sed -i -E "s#^URIs: .*#URIs: $m#" "$src"
  sudo rm -rf /var/lib/apt/lists/*
  if sudo apt-get update -q -o APT::Update::Error-Mode=any &&
     sudo apt-get install -y -q --no-install-recommends "$@"; then
    exit 0
  fi
  echo "::warning::apt failed with $m, next mirror"
done
echo "::error::no Ubuntu mirror could install: $*"
exit 1
