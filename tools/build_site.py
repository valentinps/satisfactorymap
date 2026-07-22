"""Assemble the deployable static site into dist/.

Layout:
  dist/
    index.html, *.js, *.css, vendor/, icons/   (from map/static/map/)
    tiles/{0..3}/{x}_{y}.png                   (pyramid cut from map_highres.png)
    pkg/sav_wasm.js, pkg/sav_wasm_bg.wasm      (wasm-pack --target no-modules)
    _headers                                   (Cloudflare Pages: COOP/COEP)

Prerequisites: game_data extracted (game_data/generated/*.json +
map_highres.png; see README), Rust toolchain + wasm-pack.

Usage: py tools/build_site.py [--skip-wasm]
"""

import hashlib
import os
import shutil
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DIST = os.path.join(REPO, "dist")
STATIC = os.path.join(REPO, "map", "static", "map")

# Never copied into dist/ (server-era or landing-page files).
EXCLUDE = {"landing.html", "landing.js", "landing.css", "__pycache__"}

HEADERS = """/*
  Cross-Origin-Opener-Policy: same-origin
  Cross-Origin-Embedder-Policy: require-corp
"""

TILE_SIZE = 256
# Pyramid levels: tiles/0 = 1024px overview ... tiles/3 = 8192px native, i.e.
# Leaflet zooms -3..0 (map.js adds 3 to the zoom to pick the directory).
TILE_LEVELS = 4
# Each tile carries this many pixels of its neighbors' content on every edge
# (so a tile image is TILE_SIZE + 2*TILE_BLEED square). map.js draws tiles
# oversized by the same amount so adjacent tiles overlap with identical
# pixels: fractional scaling (browser zoom, pinch, DPI) rounds each tile's
# box independently and can otherwise open sub-pixel cracks that show the
# page background through as a dark grid.
TILE_BLEED = 1


def buildTiles(mapImage):
    """Cut map_highres.png into a 256px tile pyramid.

    A single 8192px <img> overlay froze the page ~0.3-0.4s on the first visit
    to every raster scale: Chrome's GPU image-decode cache re-decodes the
    whole 17MB PNG per mip level, and the compositor commit blocks the main
    thread on it (and cache eviction re-arms the stall mid-session). 256px
    tiles decode individually off-thread, so no single decode can stall a
    frame. Tiles are cached next to the source image and rebuilt only when
    it changes.
    """
    from PIL import Image

    cacheDir = os.path.join(os.path.dirname(mapImage), "map_tiles")
    stampPath = os.path.join(cacheDir, ".stamp")
    # Content hash, not mtime: package_game_data.py ships the tile cache inside
    # game_data.zip, and zip extraction does not preserve mtimes -- an mtime
    # stamp mismatched after every unpack, re-cutting all tiles on every fresh
    # setup (and needing Pillow when the docs imply it's extraction-only).
    with open(mapImage, "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    stamp = f"{digest}:{TILE_SIZE}:{TILE_LEVELS}:{TILE_BLEED}"
    try:
        with open(stampPath, encoding="utf-8") as f:
            if f.read() == stamp:
                return cacheDir
    except OSError:
        pass

    print("cutting map tile pyramid (source image changed)...")
    if os.path.isdir(cacheDir):
        shutil.rmtree(cacheDir)
    image = Image.open(mapImage).convert("RGB")
    for level in range(TILE_LEVELS - 1, -1, -1):
        size = (TILE_SIZE * 4) << level  # 8192, 4096, 2048, 1024
        scaled = image if image.size == (size, size) else image.resize(
            (size, size), Image.LANCZOS)
        # Surround the level with a replicated 1px border so the bleed crops
        # below stay in-bounds on edge tiles too (extending the map's outer
        # edge by TILE_BLEED px, same as CSS clamping would).
        b = TILE_BLEED
        padded = Image.new("RGB", (size + 2 * b, size + 2 * b))
        padded.paste(scaled, (b, b))
        padded.paste(scaled.crop((0, 0, size, b)), (b, 0))                    # top
        padded.paste(scaled.crop((0, size - b, size, size)), (b, size + b))   # bottom
        padded.paste(scaled.crop((0, 0, b, size)), (0, b))                    # left
        padded.paste(scaled.crop((size - b, 0, size, size)), (size + b, b))   # right
        padded.paste(scaled.crop((0, 0, b, b)), (0, 0))
        padded.paste(scaled.crop((size - b, 0, size, b)), (size + b, 0))
        padded.paste(scaled.crop((0, size - b, b, size)), (0, size + b))
        padded.paste(scaled.crop((size - b, size - b, size, size)), (size + b, size + b))
        levelDir = os.path.join(cacheDir, str(level))
        os.makedirs(levelDir)
        tilesPerSide = size // TILE_SIZE
        for ty in range(tilesPerSide):
            for tx in range(tilesPerSide):
                # In padded coordinates, tile (tx, ty)'s core starts at
                # (tx*TILE_SIZE + b); backing up by b lands on tx*TILE_SIZE.
                tile = padded.crop((tx * TILE_SIZE, ty * TILE_SIZE,
                                    tx * TILE_SIZE + TILE_SIZE + 2 * b,
                                    ty * TILE_SIZE + TILE_SIZE + 2 * b))
                tile.save(os.path.join(levelDir, f"{tx}_{ty}.png"))
    with open(stampPath, "w", encoding="utf-8") as f:
        f.write(stamp)
    return cacheDir


def main():
    skip_wasm = "--skip-wasm" in sys.argv

    # Clear dist/'s CONTENTS rather than the directory itself: on Windows a
    # process whose cwd is dist/ (an old dev server, an open terminal) locks
    # the directory inode, and rmtree of the root dies with WinError 32 --
    # deleting the entries inside is still allowed.
    os.makedirs(DIST, exist_ok=True)
    for name in os.listdir(DIST):
        if skip_wasm and name == "pkg":
            continue  # --skip-wasm reuses the previously built wasm package.
        path = os.path.join(DIST, name)
        if os.path.isdir(path):
            shutil.rmtree(path)
        else:
            os.remove(path)

    # Frontend static files.
    for name in os.listdir(STATIC):
        if name in EXCLUDE:
            continue
        src = os.path.join(STATIC, name)
        dst = os.path.join(DIST, name)
        if os.path.isdir(src):
            shutil.copytree(src, dst)
        else:
            shutil.copy2(src, dst)

    # Map background: a tile pyramid cut from the 8192px source image (the
    # image itself is no longer shipped -- see buildTiles for why).
    mapImage = os.path.join(REPO, "game_data", "generated", "map_highres.png")
    if not os.path.isfile(mapImage):
        sys.exit("map_highres.png missing -- extract game data first (see README)")
    tileCache = buildTiles(mapImage)
    shutil.copytree(tileCache, os.path.join(DIST, "tiles"),
                    ignore=shutil.ignore_patterns(".stamp"))

    # WASM package. wasm-pack (and the cargo it spawns) live in ~/.cargo/bin,
    # which isn't always on PATH -- resolve it ourselves and pass an env with
    # cargo's bin dir prepended so this works from any shell.
    if not skip_wasm:
        cargo_bin = os.path.join(os.path.expanduser("~"), ".cargo", "bin")
        env = dict(os.environ, PATH=cargo_bin + os.pathsep + os.environ.get("PATH", ""))
        # SIMD (zlib-rs has simd128 paths) + bulk-memory (fast memory.copy).
        # Supported by every browser that runs wasm-bindgen output anyway.
        env["RUSTFLAGS"] = (env.get("RUSTFLAGS", "") + " -C target-feature=+simd128,+bulk-memory").strip()
        wasm_pack = shutil.which("wasm-pack", path=env["PATH"])
        if wasm_pack is None:
            sys.exit(
                "wasm-pack not found (looked on PATH and in ~/.cargo/bin).\n"
                "Install the Rust toolchain (https://rustup.rs/) and then:\n"
                "    cargo install wasm-pack"
            )
        subprocess.run(
            [wasm_pack, "build", os.path.join(REPO, "rust_parser", "wasm"),
             "--release", "--target", "no-modules",
             "--out-dir", os.path.join(DIST, "pkg"), "--out-name", "sav_wasm",
             "--no-typescript"],
            check=True, env=env,
        )
        # wasm-pack drops package.json/.gitignore into out-dir; not needed.
        for junk in ("package.json", ".gitignore", "README.md", "LICENSE"):
            path = os.path.join(DIST, "pkg", junk)
            if os.path.isfile(path):
                os.remove(path)

    with open(os.path.join(DIST, "_headers"), "w", encoding="utf-8", newline="\n") as f:
        f.write(HEADERS)

    total = 0
    for root, _dirs, files in os.walk(DIST):
        for name in files:
            total += os.path.getsize(os.path.join(root, name))
    print(f"dist/ ready: {total / 1e6:.1f} MB")


if __name__ == "__main__":
    main()
