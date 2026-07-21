"""Cut a desktop release: build, sign, checksum, draft on GitHub.

One command replaces the by-hand flow used for v0.1.0-v0.1.3:

  py tools/release.py --notes path/to/notes.md [--skip-site-build]

- Reads the version from rust_parser/tauri/tauri.conf.json.
- Builds dist/ (full wasm build unless --skip-site-build) and the Tauri
  bundles, signed with the updater key (see KEY_PATH -- the private key
  lives OUTSIDE the repo; its .sig outputs are what the in-app updater
  verifies).
- Computes SHA-256 checksums and substitutes them for the literal
  {CHECKSUMS} placeholder in the notes file.
- Writes latest.json (the updater manifest the app polls at
  releases/latest/download/latest.json).
- Creates a DRAFT GitHub release tagged v<version> and uploads the MSI,
  the NSIS setup exe, and latest.json. Publishing stays a human step.

Auth: uses the git credential manager token (never printed).
"""

import argparse
import datetime
import hashlib
import json
import os
import subprocess
import sys
import urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GH_REPO = "valentinps/satisfactorymap"
KEY_PATH = os.path.expanduser("~/.tauri/satisfactorymap.key")
BUNDLE_DIR = os.path.join(REPO, "rust_parser", "target", "release", "bundle")


def run(cmd, **kw):
    print("+", " ".join(cmd))
    subprocess.run(cmd, check=True, **kw)


def gh_token():
    out = subprocess.run(
        ["git", "credential", "fill"], cwd=REPO, capture_output=True, text=True,
        input="protocol=https\nhost=github.com\n\n").stdout
    for line in out.splitlines():
        if line.startswith("password="):
            return line.split("=", 1)[1]
    sys.exit("no GitHub token from git credential fill")


def gh(token, method, url, data=None, content_type="application/json"):
    req = urllib.request.Request(url, method=method, data=data)
    req.add_header("Authorization", f"Bearer {token}")
    req.add_header("Accept", "application/vnd.github+json")
    if data is not None:
        req.add_header("Content-Type", content_type)
    with urllib.request.urlopen(req) as resp:
        return json.load(resp)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--notes", required=True, help="release-notes .md ({CHECKSUMS} is replaced)")
    ap.add_argument("--skip-site-build", action="store_true",
                    help="reuse dist/ as-is (frontend/wasm unchanged)")
    args = ap.parse_args()

    conf = json.load(open(os.path.join(REPO, "rust_parser", "tauri", "tauri.conf.json")))
    version = conf["version"]
    product = conf["productName"]
    if not os.path.isfile(KEY_PATH):
        sys.exit(f"updater signing key missing: {KEY_PATH}")
    notes = open(args.notes, encoding="utf-8").read()

    if not args.skip_site_build:
        run([sys.executable, os.path.join(REPO, "tools", "build_site.py")])
    env = dict(os.environ,
               TAURI_SIGNING_PRIVATE_KEY_PATH=KEY_PATH,
               TAURI_SIGNING_PRIVATE_KEY_PASSWORD="")
    run(["cargo", "tauri", "build"], cwd=os.path.join(REPO, "rust_parser", "tauri"), env=env)

    msi = os.path.join(BUNDLE_DIR, "msi", f"{product}_{version}_x64_en-US.msi")
    exe = os.path.join(BUNDLE_DIR, "nsis", f"{product}_{version}_x64-setup.exe")
    sig = exe + ".sig"
    for path in (msi, exe, sig):
        if not os.path.isfile(path):
            sys.exit(f"expected bundle artifact missing: {path}")

    clean = product.replace(" ", "-")
    assets = {  # upload name -> local path
        f"{clean}_{version}_x64_en-US.msi": msi,
        f"{clean}_{version}_x64-setup.exe": exe,
    }

    checks = []
    for name, path in assets.items():
        digest = hashlib.sha256(open(path, "rb").read()).hexdigest()
        checks.append(f"{digest}  {name}")
    notes = notes.replace("{CHECKSUMS}", "\n".join(checks))

    # Updater manifest: the app polls releases/latest/download/latest.json,
    # so the URL must point at THIS release's renamed setup exe.
    manifest = {
        "version": version,
        "notes": f"See https://github.com/{GH_REPO}/releases/tag/v{version}",
        "pub_date": datetime.datetime.now(datetime.timezone.utc)
                    .strftime("%Y-%m-%dT%H:%M:%SZ"),
        "platforms": {
            "windows-x86_64": {
                "signature": open(sig, encoding="utf-8").read(),
                "url": f"https://github.com/{GH_REPO}/releases/download/"
                       f"v{version}/{clean}_{version}_x64-setup.exe",
            }
        },
    }
    manifest_path = os.path.join(BUNDLE_DIR, "latest.json")
    with open(manifest_path, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)
    assets["latest.json"] = manifest_path

    token = gh_token()
    release = gh(token, "POST", f"https://api.github.com/repos/{GH_REPO}/releases",
                 json.dumps({"tag_name": f"v{version}",
                             "target_commitish": "main",
                             "name": f"v{version}",
                             "body": notes,
                             "draft": True}).encode())
    print(f"draft release id {release['id']}: {release['html_url']}")
    for name, path in assets.items():
        uploaded = gh(token, "POST",
                      f"https://uploads.github.com/repos/{GH_REPO}/releases/"
                      f"{release['id']}/assets?name={name}",
                      open(path, "rb").read(), "application/octet-stream")
        print(f"  asset {uploaded['name']}: {uploaded['state']} ({uploaded['size']} bytes)")
    print("\nDraft ready -- review and publish on GitHub. Publishing makes")
    print("this latest.json the updater manifest every installed app sees.")


if __name__ == "__main__":
    main()
