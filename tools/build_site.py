"""Assemble the deployable static site into dist/.

Layout:
  dist/
    index.html, *.js, *.css, vendor/, icons/   (from map/static/map/)
    map_highres.png                            (from game_data/generated/)
    pkg/sav_wasm.js, pkg/sav_wasm_bg.wasm      (wasm-pack --target no-modules)
    _headers                                   (Cloudflare Pages: COOP/COEP)

Prerequisites: game_data extracted (game_data/generated/*.json +
map_highres.png; see README), Rust toolchain + wasm-pack.

Usage: py tools/build_site.py [--skip-wasm]
"""

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

    # Map background image.
    mapImage = os.path.join(REPO, "game_data", "generated", "map_highres.png")
    if not os.path.isfile(mapImage):
        sys.exit("map_highres.png missing -- extract game data first (see README)")
    shutil.copy2(mapImage, os.path.join(DIST, "map_highres.png"))

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
