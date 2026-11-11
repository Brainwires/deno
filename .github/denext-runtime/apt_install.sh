#!/usr/bin/env bash
# Copyright 2018-2026 the Deno authors. MIT license.
# Installs Debian packages for a CI job without depending on one good apt run.
#
#   apt_install.sh [--with-recommends] <package>...
#
# - A package already installed is skipped, and with every package present
#   nothing is fetched at all: our self-hosted Linux images (biscuits'
#   denext-ci-linux and denext-ci-linux-2204) carry every package these jobs
#   name, so a job there downloads nothing.
# - Otherwise apt runs with its own retries and timeouts (Acquire::Retries,
#   Acquire::http(s)::Timeout, a dpkg lock wait), each `apt-get update` /
#   `install` under `timeout`, and the whole try is repeated with a backoff.
#   From the third try on, the Ubuntu archive sources point at a fallback
#   mirror (GitHub's images use azure.archive.ubuntu.com, whose lookups have
#   failed with exit 100), so one mirror or one DNS name can't fail the job.
#
# Env: APT_INSTALL_TRIES (default 4), APT_INSTALL_MIRRORS (space-separated
# fallback archive URLs, tried in turn; default per architecture),
# APT_INSTALL_DELAY (the first backoff, seconds; default 15), APT_ETC (default
# /etc/apt; the tests point it elsewhere).
set -euo pipefail

recommends=(--no-install-recommends)
if [ "${1:-}" = --with-recommends ]; then
  recommends=()
  shift
fi
if [ "$#" -eq 0 ]; then
  echo "::error::apt_install.sh: no packages named" >&2
  exit 1
fi

missing=()
for pkg in "$@"; do
  if [ "$(dpkg-query -W -f='${db:Status-Status}' "$pkg" 2>/dev/null || true)" != installed ]; then
    missing+=("$pkg")
  fi
done
if [ "${#missing[@]}" -eq 0 ]; then
  echo "apt_install.sh: already installed, nothing to fetch: $*"
  exit 0
fi
echo "apt_install.sh: installing ${missing[*]}"

sudo=
if [ "$(id -u)" != 0 ]; then sudo=sudo; fi
export DEBIAN_FRONTEND=noninteractive

apt_opts=(
  -o Acquire::Retries=5
  -o Acquire::http::Timeout=30
  -o Acquire::https::Timeout=30
  -o DPkg::Lock::Timeout=300
  -o Dpkg::Use-Pty=0
)

case "$(dpkg --print-architecture)" in
  amd64 | i386)
    default_mirrors="http://archive.ubuntu.com/ubuntu http://mirrors.edge.kernel.org/ubuntu"
    ;;
  *)
    default_mirrors="http://ports.ubuntu.com/ubuntu-ports"
    ;;
esac
read -r -a mirrors <<<"${APT_INSTALL_MIRRORS:-$default_mirrors}"
etc=${APT_ETC:-/etc/apt}

# Points every Ubuntu archive / security / ports source at $1 (both the
# one-line sources.list format and 24.04's deb822 .sources files).
use_mirror() {
  local url=$1 f
  for f in "$etc/sources.list" "$etc"/sources.list.d/*.list "$etc"/sources.list.d/*.sources; do
    [ -f "$f" ] || continue
    # -i with a suffix: the same on GNU and BSD sed.
    $sudo sed -E -i.apt-install-bak \
      "s#https?://([a-z0-9.-]+\.)?(archive|security)\.ubuntu\.com/ubuntu/?#$url/#g; s#https?://([a-z0-9.-]+\.)?ports\.ubuntu\.com/ubuntu-ports/?#$url/#g; s#https?://mirrors\.edge\.kernel\.org/ubuntu/?#$url/#g" \
      "$f"
    $sudo rm -f "$f.apt-install-bak"
  done
  echo "apt_install.sh: apt sources now use $url"
}

# man-db triggers can take minutes on hosted runners.
$sudo apt-get -qq remove --purge -y man-db >/dev/null 2>&1 || true

tries=${APT_INSTALL_TRIES:-4}
delay=${APT_INSTALL_DELAY:-15}
for ((try = 1; try <= tries; try++)); do
  if [ "$try" -ge 3 ] && [ "${#mirrors[@]}" -gt 0 ]; then
    use_mirror "${mirrors[$(((try - 3) % ${#mirrors[@]}))]}"
  fi
  if timeout 600 $sudo apt-get "${apt_opts[@]}" update -q &&
    timeout 1200 $sudo apt-get "${apt_opts[@]}" install -y ${recommends[@]+"${recommends[@]}"} "${missing[@]}"; then
    exit 0
  fi
  if [ "$try" -lt "$tries" ]; then
    echo "::warning::apt_install.sh: try $try of $tries failed; again in ${delay}s"
    sleep "$delay"
    delay=$((delay * 2))
  fi
done
echo "::error::apt_install.sh: could not install ${missing[*]} after $tries tries (the last two on a fallback mirror)" >&2
exit 1
