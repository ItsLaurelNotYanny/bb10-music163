#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
IMAGE=bb10-native-dev:local

docker image inspect "$IMAGE" >/dev/null

mkdir -p "$ROOT/build/bb10"
docker run --rm --network none --user "$(id -u):$(id -g)" \
    -v "$ROOT:/src" -w /src "$IMAGE" \
    sh -c '
        set -eu
        cmake -S /src -B /src/build/bb10 \
            -DCMAKE_TOOLCHAIN_FILE=/opt/bb10-toolchain.cmake \
            -DCMAKE_BUILD_TYPE=Release \
            -DPASSPORT_BB10=ON
        cmake --build /src/build/bb10 -j2
        /opt/bbndk/host/linux/x86/usr/bin/blackberry-nativepackager -devMode \
            -package /src/build/bb10/bb10-music163.bar \
            /src/bar-descriptor.xml -configuration Device-Debug
    '

python3 "$ROOT/scripts/check-bar.py" "$ROOT/build/bb10/bb10-music163.bar"

VERSION=$(python3 -c 'import xml.etree.ElementTree as ET; r=ET.parse("'"$ROOT/bar-descriptor.xml"'").getroot(); el=r.find("{http://www.qnx.com/schemas/application/1.0}versionNumber"); print(el.text.strip() if el is not None and el.text else "1.0.0")')
BUILD=$(python3 -c 'import xml.etree.ElementTree as ET; r=ET.parse("'"$ROOT/bar-descriptor.xml"'").getroot(); el=r.find("{http://www.qnx.com/schemas/application/1.0}buildId"); print(el.text.strip() if el is not None and el.text else "1")')
VERSIONED_BAR="$ROOT/build/bb10/bb10-music163-v${VERSION}-build${BUILD}.bar"
cp -f "$ROOT/build/bb10/bb10-music163.bar" "$VERSIONED_BAR"
echo "Versioned BAR: $VERSIONED_BAR"
