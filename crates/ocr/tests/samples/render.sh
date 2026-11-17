#!/bin/sh
# Renders the sample images from the HTML in src/ with headless Chrome. Each line names the
# sample, its CSS size and its device scale factor (2 is a Retina capture, 1 a standard display).
# The codes come from Core Image: `swift src/codes.swift qr <payload> src/qr-code.png`.
set -eu
cd "$(dirname "$0")"
CHROME="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
render() {
  name=$1 size=$2 scale=$3
  profile=$(mktemp -d)
  "$CHROME" --headless=new --disable-gpu --hide-scrollbars --no-first-run --user-data-dir="$profile" \
    --force-device-scale-factor="$scale" --window-size="$size" --screenshot="$PWD/$name.png" \
    "file://$PWD/src/$name.html" >/dev/null 2>&1
  rm -rf "$profile"
}
render en-paragraph 640,250 2
render tr-paragraph 640,250 2
render two-column 760,420 2
render table 520,300 2
render code 720,330 2
render terminal 600,250 2
render small-ui 220,112 1
render qr 520,230 2
render ui-sidebar 680,420 2
render tr-dark 520,220 2
render tr-small 400,112 1
