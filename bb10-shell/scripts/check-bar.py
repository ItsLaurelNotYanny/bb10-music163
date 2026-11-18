#!/usr/bin/env python3
"""Verify the packaged BB10 Cascades WebView shell BAR archive."""

import base64
import hashlib
from pathlib import Path
import struct
import sys
import zipfile


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    package = Path(sys.argv[1]) if len(sys.argv) > 1 else (root / "build/bb10/bb10-music163.bar")
    if not package.is_file():
        raise SystemExit(f"Missing BAR package: {package}")

    expected_permissions = {"run_native", "access_internet"}

    with zipfile.ZipFile(package) as archive:
        assert archive.testzip() is None, "Corrupt BAR archive"
        names = archive.namelist()
        assert not any(n.endswith((".apk", ".dex")) for n in names), "Android payload forbidden"
        assert len(names) == len(set(names)), "Duplicate BAR entries"

        manifest = archive.read("META-INF/MANIFEST.MF").decode("utf-8").replace("\r\n", "\n")
        fields = dict(line.split(": ", 1) for line in manifest.splitlines() if ": " in line)

        user_actions = [p for p in fields.get("Entry-Point-User-Actions", "").split(",") if p]
        sys_actions = [p for p in fields.get("Entry-Point-System-Actions", "").split(",") if p]
        permissions = set(user_actions) | set(sys_actions)
        assert permissions == expected_permissions, f"Unexpected permissions: {permissions!r}"
        assert fields.get("Package-Architecture") == "armle-v7", "Expected armle-v7"
        assert fields.get("Package-Name") == "com.bb10music163.shell", "Unexpected Package-Name"

        app_icon = archive.read("native/assets/icon.png")
        assert app_icon == (root / "assets/icon.png").read_bytes(), "Packaged icon mismatch"
        assert app_icon[:8] == b"\x89PNG\r\n\x1a\n", "Expected PNG signature"
        assert struct.unpack_from(">II", app_icon, 16) == (144, 144), "Expected 144x144 icon"
        assert app_icon[24:26] == bytes((8, 6)), "Expected 8-bit RGBA icon"

        main_qml = archive.read("native/assets/main.qml")
        assert main_qml == (root / "assets/main.qml").read_bytes(), "main.qml mismatch"

        for block in manifest.split("\n\n"):
            data = dict(
                line.split(": ", 1)
                for line in block.replace("\n ", "").splitlines()
                if ": " in line
            )
            if "Archive-Asset-Name" in data:
                raw_asset = archive.read(data["Archive-Asset-Name"])
                digest = base64.urlsafe_b64encode(hashlib.sha512(raw_asset).digest()).decode("ascii").rstrip("=")
                assert digest == data["Archive-Asset-SHA-512-Digest"], (
                    f"SHA-512 mismatch for {data['Archive-Asset-Name']}"
                )

        elf_files = [n for n in names if not n.endswith("/") and archive.read(n).startswith(b"\x7fELF")]
        assert elf_files == ["native/bb10-music163"], f"Unexpected ELF files: {elf_files!r}"

        binary = archive.read("native/bb10-music163")
        assert binary[:4] == b"\x7fELF" and binary[4:6] == b"\x01\x01", "Expected 32-bit LSB ELF"
        assert struct.unpack_from("<H", binary, 18)[0] == 40, "Expected ARM (EM_ARM=40) ELF"

    print(f"OK: {package} ({package.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
