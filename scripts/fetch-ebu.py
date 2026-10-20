#!/usr/bin/env python3
"""Fetch the unchanged EBU v05 archive, with a verified mirror fallback."""

import hashlib
from pathlib import Path
import subprocess
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / "fixtures" / "ebu"
NAME = "ebu-loudness-test-setv05.zip"
SIZE = 91631421
# Published by Gentoo's media-libs/libebur128/Manifest for the EBU v05 zip.
SHA512 = (
    "60d022fdac47ad0be2688411be9daecbff85da994d6fa4921bba6cffab841b081d8"
    "b15d9ce284ad2253efb686463450a84a0d19cb0bad7a934546cc52dd73771"
)
URLS = [
    "https://tech.ebu.ch/files/live/sites/tech/files/shared/testmaterial/" + NAME,
    "https://distfiles.gentoo.org/distfiles/de/" + NAME,
]


def verified(path):
    if not path.is_file() or path.stat().st_size != SIZE:
        return False
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha512").hexdigest() == SHA512


def main():
    DEST.mkdir(parents=True, exist_ok=True)
    archive = DEST / NAME
    if not verified(archive):
        with tempfile.TemporaryDirectory(prefix="download-", dir=DEST) as work:
            candidate = Path(work) / NAME
            for url in URLS:
                print(f"Fetching {url}", flush=True)
                result = subprocess.run([
                    "curl", "--fail", "--location", "--silent", "--show-error",
                    "--retry", "2", "--connect-timeout", "20", "--max-time", "300",
                    "--output", str(candidate), url,
                ])
                if result.returncode == 0 and verified(candidate):
                    candidate.replace(archive)
                    break
                print("Download failed or size/SHA-512 mismatch", flush=True)
            else:
                print("SKIP EBU fetch: archive unavailable; required oracle tests will fail", flush=True)
                return
    print(f"Verified EBU v05: {SIZE} bytes; SHA-512 {SHA512}", flush=True)
    with zipfile.ZipFile(archive) as source:
        for member in source.infolist():
            relative = Path(member.filename)
            if relative.is_absolute() or ".." in relative.parts:
                raise ValueError(f"Unsafe archive path: {relative}")
            target = DEST / relative
            if not target.is_file() or target.stat().st_size != member.file_size:
                source.extract(member, DEST)
    print("EBU Loudness Test Set v05 ready in fixtures/ebu/", flush=True)


if __name__ == "__main__":
    main()
