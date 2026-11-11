#!/usr/bin/env bash
# Points apt on a GitHub runner at one working Ubuntu mirror.
# Canonical's servers (archive, security, ports) send runners in Azure to
# azure.archive.ubuntu.com and hang when that is down, and apt's mirror lists spread
# requests over every entry, so one dead entry stalls the whole update. Instead: probe
# independent mirrors in order and use the first that answers, Canonical never.
set -euo pipefail
src=/etc/apt/sources.list.d/ubuntu.sources
if grep -q ubuntu-ports "$src"; then
  candidates="http://mirrors.ocf.berkeley.edu/ubuntu-ports/ http://mirrors.mit.edu/ubuntu-ports/ http://ftp.tsukuba.wide.ad.jp/Linux/ubuntu-ports/"
else
  candidates="http://mirrors.edge.kernel.org/ubuntu/ http://mirrors.ocf.berkeley.edu/ubuntu/ http://mirrors.mit.edu/ubuntu/"
fi
. /etc/os-release
for m in $candidates; do
  if curl -fsS --max-time 10 -o /dev/null "${m}dists/${VERSION_CODENAME}-security/InRelease"; then
    echo "apt mirror: $m"
    sudo sed -i -E "s#^URIs: .*#URIs: $m#" "$src"
    printf 'Acquire::Retries "3";\nAcquire::http::Timeout "20";\n' | sudo tee /etc/apt/apt.conf.d/80-retries > /dev/null
    exit 0
  fi
  echo "apt mirror $m does not answer, next"
done
echo "::warning::no independent Ubuntu mirror answered; keeping the runner's own"
