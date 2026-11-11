#!/usr/bin/env bash
# Points apt on a GitHub runner at a list of Ubuntu mirrors, tried in order.
# Canonical's own servers (archive, security, ports) send requests from Azure to
# azure.archive.ubuntu.com; when that mirror is down, every job that installs packages
# hangs on apt-get until it times out. Independent mirrors go first, Canonical last.
set -euo pipefail
src=/etc/apt/sources.list.d/ubuntu.sources
if grep -q ubuntu-ports "$src"; then
  path=ubuntu-ports
  canonical=http://ports.ubuntu.com/ubuntu-ports/
  mirrors="http://mirrors.ocf.berkeley.edu/$path/ http://mirrors.mit.edu/$path/"
else
  path=ubuntu
  canonical=http://archive.ubuntu.com/ubuntu/
  mirrors="http://mirrors.edge.kernel.org/$path/ http://mirrors.ocf.berkeley.edu/$path/ http://mirrors.mit.edu/$path/"
fi
printf '%s\n' $mirrors "$canonical" | sudo tee /etc/apt/manydesk-mirrors.txt
sudo sed -i -E 's#^URIs: .*#URIs: mirror+file:/etc/apt/manydesk-mirrors.txt#' "$src"
printf 'Acquire::Retries "3";\nAcquire::http::Timeout "20";\nAcquire::https::Timeout "20";\n' | sudo tee /etc/apt/apt.conf.d/80-retries > /dev/null
grep -n '^URIs' "$src"
