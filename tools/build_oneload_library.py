#!/usr/bin/env python3
"""Build a folder-per-game Game Mode library from a OneLoad64 collection.

OneLoad64 ships as a flat pile of `<Game>.crt` cartridges at the collection
root, with art stored centrally and keyed by the *exact* game basename:

    <root>/<Game>.crt
    <root>/Extras/Images/LoadingScreens/<Game>.png   (box-art-ish, ~56% of games)
    <root>/Extras/Images/Screenshots/<Game>.png      (in-game shot, ~100%)

This script turns that into a self-contained, portable library where every
game sits in its own folder with the matched art baked in as siblings:

    <out>/<Game>/<Game>.crt
    <out>/<Game>/cover.png        <- loading screen, or the screenshot if none
    <out>/<Game>/screenshot.png   <- the in-game screenshot

Those `cover.png` / `screenshot.png` names are exactly what the app's Game Mode
looks for, so pointing it at <out> shows real box art + screenshots with no
central-folder probing — and it works in any other tool too.

Re-run it whenever a new OneLoad64 release drops.

Usage:
    build_oneload_library.py <oneload_root> [<out_dir>]

    <oneload_root>  the extracted "OneLoad64-Games-Collection-vN" folder
    <out_dir>       where to build (default: "<oneload_root> Arted" next to it)

Options via env:
    LINK=1          hard-link files instead of copying (instant, no extra disk;
                    same filesystem only)
"""

from __future__ import annotations

import os
import shutil
import sys


def find_art_dir(root: str, *names: str) -> str | None:
    """Return the first existing art folder, trying a couple of known layouts."""
    for rel in names:
        p = os.path.join(root, rel)
        if os.path.isdir(p):
            return p
    return None


def place(src: str, dst: str, link: bool) -> None:
    if os.path.exists(dst):
        os.remove(dst)
    if link:
        try:
            os.link(src, dst)
            return
        except OSError:
            pass  # cross-device or unsupported → fall back to copy
    shutil.copyfile(src, dst)


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2

    root = os.path.abspath(os.path.expanduser(sys.argv[1]))
    if not os.path.isdir(root):
        print(f"error: not a directory: {root}")
        return 1

    out = (
        os.path.abspath(os.path.expanduser(sys.argv[2]))
        if len(sys.argv) > 2
        else root.rstrip("/") + " Arted"
    )
    link = os.environ.get("LINK") == "1"

    loads = find_art_dir(root, "Extras/Images/LoadingScreens", "LoadingScreens")
    shots = find_art_dir(root, "Extras/Images/Screenshots", "Screenshots")
    if not shots and not loads:
        print("warning: no LoadingScreens/Screenshots art folders found — "
              "games will have no covers.")

    games = sorted(f for f in os.listdir(root) if f.lower().endswith(".crt"))
    if not games:
        print(f"error: no .crt files at the collection root: {root}")
        return 1

    os.makedirs(out, exist_ok=True)
    made = with_cover = with_shot = missing = 0

    for f in games:
        name = f[:-4]  # basename without .crt
        dst = os.path.join(out, name)
        os.makedirs(dst, exist_ok=True)
        place(os.path.join(root, f), os.path.join(dst, f), link)
        made += 1

        load_png = os.path.join(loads, name + ".png") if loads else ""
        shot_png = os.path.join(shots, name + ".png") if shots else ""
        has_load = load_png and os.path.exists(load_png)
        has_shot = shot_png and os.path.exists(shot_png)

        # Box art: prefer the loading screen, fall back to the screenshot.
        box = load_png if has_load else (shot_png if has_shot else None)
        if box:
            place(box, os.path.join(dst, "cover.png"), link)
            with_cover += 1
        if has_shot:
            place(shot_png, os.path.join(dst, "screenshot.png"), link)
            with_shot += 1
        if not box and not has_shot:
            missing += 1

        if made % 500 == 0:
            print(f"  … {made}/{len(games)}")

    print(f"\nDone → {out}")
    print(f"  games:        {made}")
    print(f"  with box art: {with_cover}")
    print(f"  with shot:    {with_shot}")
    if missing:
        print(f"  no art:       {missing}")
    print(f"  mode:         {'hard-link' if link else 'copy'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
