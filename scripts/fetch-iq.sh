#!/usr/bin/env bash
# Download local IQ recordings into tests/data (gitignored).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
data=$root/tests/data
mkdir -p "$data"

modes1=$data/modes1.bin
modes1_url=https://raw.githubusercontent.com/antirez/dump1090/master/testfiles/modes1.bin
modes1_sha=3a33e16025da8669149c780075950b4e908ca036ea21f9583c113f60d5fb3094

if [[ ! -f $modes1 ]]; then
  echo "fetch $modes1_url"
  curl -fL --retry 3 --retry-delay 2 -o "$modes1" "$modes1_url"
fi
echo "$modes1_sha  $modes1" | sha256sum -c -
bytes=$(wc -c <"$modes1")
if [[ $bytes -ne 713736 ]]; then
  echo "modes1.bin size $bytes, expected 713736" >&2
  exit 1
fi

zenodo_api=https://zenodo.org/api/records/23342598
zip=$data/AWEC2026_ADSB_Data.zip
zip_md5=abd448848ef0875f2df071d8f089680d
out=$data/awec2026

echo "zenodo $zenodo_api"
if ! url=$(curl -fL --retry 3 --retry-delay 2 "$zenodo_api" | python3 -c 'import json,sys; print(json.load(sys.stdin)["files"][0]["links"]["self"])'); then
  echo "zenodo record API failed; ignored AWEC tests will skip" >&2
  exit 0
fi
echo "zenodo file $url"
if ! curl -fL --retry 3 --retry-delay 2 -o "$zip" "$url"; then
  echo "zenodo download failed (often HTTP 403); ignored AWEC tests will skip" >&2
  rm -f "$zip"
  exit 0
fi
echo "$zip_md5  $zip" | md5sum -c -
rm -rf "$out"
mkdir -p "$out"
unzip -o -q "$zip" -d "$out"
echo "ok $data"
