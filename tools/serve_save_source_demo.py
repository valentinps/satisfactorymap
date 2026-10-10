"""Run the app with a stubbed desktop bridge, so the "Save on another machine"
dialog (map/static/map/save_source.js) can be clicked through without a Tauri
build and without a game machine.

    py tools/serve_save_source_demo.py [port] [--dir FOLDER]

Serves map/static/map/ and injects a script that installs window.__TAURI__ with
the three commands the dialog uses (source_validate, source_browse,
source_fetch). FOLDER -- the current directory by default -- is what the dialog
browses, so the listing, its sizes and timestamps, the "newest .sav" pick and
the watch's change check are all real; only the SSH transport is canned.

Loading is where the demo stops: the parser lives in sav_core (desktop build) or
in dist/pkg (wasm build) and this server has neither, so the dialog's status
line says so instead of pretending. Validate against a host containing "fail"
to see the debug path, and call `__demoEmit("1.2_autosave_2.sav")` from the
console to see a change event reload the map (set `window.__demoFailWatch = true`
first to see the polling fallback). For the real thing see docs/remote-save-ssh.md.
"""

import argparse
import http.server
import json
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
STATIC = os.path.join(REPO, "map", "static", "map")

# Injected before the app scripts: window.__TAURI__ has to exist by the time
# save_client.js reads it, or the app takes the browser transport instead.
STUB = """<script>
(function () {
  var DIR = %(dir)s;
  // Re-read from the server on every call: that is what makes the watch loop
  // live -- touch a file and the next tick sees it.
  var listing = function () { return fetch("/__demo/listing").then(function (r) { return r.json(); }); };
  // loadSavePath builds a Channel before it invokes anything.
  function Channel() { this.onmessage = null; }
  window.__TAURI__ = { core: { Channel: Channel, invoke: function (cmd, args) {
    // A host containing "fail" shows the debug path: the client's own words and
    // the hint under them. Nothing else here depends on the host name.
    if (cmd === "source_validate" && /fail/i.test((args.target && args.target.host) || "")) {
      return Promise.reject(new Error("dion@192.168.1.50: Permission denied (publickey)."));
    }
    if (cmd === "source_validate") {
      return listing().then(function (l) {
        var saves = l.entries.filter(function (e) { return !e.isDir && /\\.sav$/i.test(e.name); });
        return { ok: true, platform: "demo", home: DIR, sftp: true, rsync: false,
          saveCount: saves.length, newestSave: saves.length ? saves[0].name : "", watch: "inotify",
          detail: "demo bridge: browsing " + DIR };
      });
    }
    if (cmd === "source_browse") { return listing(); }
    if (cmd === "source_watch_start") {
      // The demo has no inotifywait to run (this box has no inotify-tools), so
      // it reports the event path as live and lets __demoEmit stand in for the
      // kernel. Set __demoFailWatch to see the polling fallback instead.
      if (window.__demoFailWatch) {
        return Promise.reject(new Error("demo: no event watcher here"));
      }
      return Promise.resolve(args.mode || "inotify");
    }
    if (cmd === "source_watch_stop") { return Promise.resolve(); }
    if (cmd === "source_fetch") {
      return listing().then(function (l) {
        var entry = l.entries.filter(function (e) { return e.path === args.remotePath; })[0];
        if (!entry) { throw new Error("no such file: " + args.remotePath); }
        return { path: entry.path, saveName: entry.name, bytes: entry.size,
          sha256: entry.name + ":" + entry.size + ":" + entry.mtime, remotePath: entry.path };
      });
    }
    if (cmd === "load") {
      return Promise.reject(new Error("demo build has no parser (sav_core is the desktop app, dist/pkg the wasm build)"));
    }
    return Promise.reject(new Error("demo build: " + cmd + " is not stubbed"));
  } } };
  // A change event, as the Rust side would push it: __demoEmit("x.sav").
  var listeners = {};
  window.__TAURI__.event = {
    listen: function (name, fn) { listeners[name] = fn; return Promise.resolve(function () { delete listeners[name]; }); }
  };
  window.__demoEmit = function (name) {
    if (listeners["save-source-changed"]) { listeners["save-source-changed"]({ payload: name }); }
  };
  document.addEventListener("DOMContentLoaded", function () {
    var note = document.createElement("div");
    note.textContent = "demo bridge - real folder, no parser, simulated change events: tools/serve_save_source_demo.py";
    note.style.cssText = "position:fixed;left:8px;bottom:8px;z-index:9999;font:11px system-ui;"
      + "color:#ffd479;background:rgba(20,20,25,.85);border:1px solid #6a5a2a;border-radius:6px;padding:4px 8px";
    document.body.appendChild(note);
  });
})();
</script>
"""


def snapshot(folder):
    """The folder as the dialog sees it: real names, sizes and timestamps."""
    entries = []
    for item in sorted(os.scandir(folder), key=lambda e: e.name.lower()):
        try:
            stat = item.stat()
        except OSError:
            continue
        entries.append({
            "name": item.name,
            "path": item.path,
            "isDir": item.is_dir(),
            "size": 0 if item.is_dir() else stat.st_size,
            "mtime": int(stat.st_mtime),
        })
    entries.sort(key=lambda e: (not e["isDir"], not e["name"].lower().endswith(".sav"), e["name"].lower()))
    return entries


class Handler(http.server.SimpleHTTPRequestHandler):
    folder = os.getcwd()
    extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map, ".js": "text/javascript"}

    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=STATIC, **kwargs)

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/__demo/listing":
            folder = os.path.abspath(self.folder)
            body = json.dumps({
                "path": folder,
                "parent": os.path.dirname(folder),
                "entries": snapshot(folder),
            }).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()
            self.wfile.write(body)
            return
        # index.html carries the injected bridge; everything else is the app.
        if path in ("/", "/index.html"):
            with open(os.path.join(STATIC, "index.html"), encoding="utf-8") as f:
                page = f.read()
            stub = STUB % {"dir": json.dumps(os.path.abspath(self.folder))}
            page = page.replace('<script src="analytics.js"></script>', stub + '<script src="analytics.js"></script>', 1)
            body = page.encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()
            self.wfile.write(body)
            return
        super().do_GET()

    def end_headers(self):
        # The watch loop polls by re-listing; a cached stub would freeze it.
        self.send_header("Cache-Control", "no-cache")
        super().end_headers()

    def log_message(self, *args):
        pass


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("port", nargs="?", type=int, default=8793)
    parser.add_argument("--dir", default=os.getcwd(), help="folder the dialog browses (default: cwd)")
    args = parser.parse_args()
    if not os.path.isdir(args.dir):
        sys.exit("not a folder: " + args.dir)
    Handler.folder = os.path.abspath(args.dir)
    print(f"demo bridge on http://127.0.0.1:{args.port}/  (browsing {Handler.folder})")
    print("Save File -> From another machine -> Validate connection")
    with http.server.ThreadingHTTPServer(("127.0.0.1", args.port), Handler) as server:
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass


if __name__ == "__main__":
    main()
