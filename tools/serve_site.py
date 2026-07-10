"""Serve dist/ locally with the same COOP/COEP headers Cloudflare Pages
sends (tools/build_site.py writes dist/_headers for production) and a
correct application/wasm MIME type (Windows' mimetypes registry is
unreliable for .wasm).

Usage: py tools/serve_site.py [port]
"""

import http.server
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DIST = os.path.join(REPO, "dist")


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {
        **http.server.SimpleHTTPRequestHandler.extensions_map,
        ".wasm": "application/wasm",
        ".js": "text/javascript",
    }

    def __init__(self, *args, **kwargs):
        # directory= instead of os.chdir(DIST): holding dist/ as the
        # process cwd would block build_site.py's rebuild on Windows.
        super().__init__(*args, directory=DIST, **kwargs)

    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        super().end_headers()


def main():
    # Not 8080: WSL's wslrelay (and assorted proxies) squat that port and
    # intercept connections before this server sees them.
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8791
    with http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler) as server:
        print(f"Serving dist/ at http://127.0.0.1:{port}/")
        server.serve_forever()


if __name__ == "__main__":
    main()
