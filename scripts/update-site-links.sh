#!/usr/bin/env bash
# Points the website's download buttons, wget lines and install commands at
# a release's versioned .deb and .dmg. Run by the release workflow once the
# files are attached to the release (see ADR-0024); safe to run by hand.
#
# Usage: scripts/update-site-links.sh <version> [deb-revision]
set -euo pipefail

version="${1:?usage: update-site-links.sh <version> [deb-revision]}"
revision="${2:-1}"
page="$(dirname "$0")/../docs/index.html"

if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "not a version: $version" >&2
  exit 1
fi

deb="photograph_${version}-${revision}_amd64.deb"
dmg="Photograph-${version}.dmg"

sed -i -E \
  -e "s#releases/download/v[0-9.]+/photograph_[0-9.]+-[0-9]+_amd64\.deb#releases/download/v${version}/${deb}#g" \
  -e "s#\./photograph_[0-9.]+-[0-9]+_amd64\.deb#./${deb}#g" \
  -e "s#Download \.deb \(v[0-9.]+\)#Download .deb (v${version})#g" \
  -e "s#releases/download/v[0-9.]+/Photograph-[0-9.]+\.dmg#releases/download/v${version}/${dmg}#g" \
  -e "s#Download \.dmg \(v[0-9.]+\)#Download .dmg (v${version})#g" \
  "$page"

grep -q "releases/download/v${version}/${deb}" "$page" || { echo "no .deb link updated in $page" >&2; exit 1; }
grep -q "releases/download/v${version}/${dmg}" "$page" || { echo "no .dmg link updated in $page" >&2; exit 1; }
echo "Pointed $page at v${version}"
