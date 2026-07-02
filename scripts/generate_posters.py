#!/usr/bin/env python3
"""Extract a poster frame for every video specimen in the archive.

Reads <media-dir>/metadata.jsonl, writes <media-dir>/posters/<rkey>.jpg via
ffmpeg. Local-mode pages use these as video posters and archive thumbnails;
rerun after refreshing the archive.

Usage: scripts/generate_posters.py [media-dir]
"""

import json
import pathlib
import subprocess
import sys

DEFAULT_MEDIA_DIR = "/home/coreyja.linux/paperclips-media/oops"


def main() -> None:
    media_dir = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else DEFAULT_MEDIA_DIR)
    rows = [
        json.loads(line)
        for line in (media_dir / "metadata.jsonl").read_text().splitlines()
        if line.strip()
    ]
    posters = media_dir / "posters"
    posters.mkdir(exist_ok=True)

    specimens = [r for r in rows if r["kind"] == "video"]
    made = skipped = failed = 0
    for s in specimens:
        src = media_dir / s["file"]
        dst = posters / f"{s['rkey']}.jpg"
        if dst.exists():
            skipped += 1
            continue
        # Frame at 3s captures a developed sim; fall back to the first frame
        # for clips shorter than that.
        for seek in ("3", "0"):
            result = subprocess.run(
                ["ffmpeg", "-v", "error", "-ss", seek, "-i", str(src),
                 "-frames:v", "1", "-q:v", "3", str(dst), "-y"],
                capture_output=True,
            )
            if result.returncode == 0 and dst.exists() and dst.stat().st_size > 0:
                made += 1
                break
        else:
            failed += 1
            print(f"FAILED: {s['file']}", file=sys.stderr)

    print(f"posters: {made} made, {skipped} existing, {failed} failed")


if __name__ == "__main__":
    main()
