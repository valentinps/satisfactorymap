#!/usr/bin/env python3
# This file is part of the Satisfactory Save Parser distribution
#                                  (https://github.com/GreyHak/sat_sav_parse).
# Copyright (c) 2024-2026 GreyHak (github.com/GreyHak).
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, version 3.
#
# This program is distributed in the hope that it will be useful, but
# WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU
# General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program. If not, see <https://www.gnu.org/licenses/>.

# Local web server for the interactive save-file map viewer.
# Usage: py map/sav_map_server.py [--port PORT] [--host HOST] [--no-browser]
# The mode (upload / local folder / SFTP) is configured through the landing page.
#
# Dependencies: pip install flask  (pip install paramiko for SFTP mode)

import argparse
import glob
import json
import os
import sys
import threading
import time
import webbrowser

try:
   import flask
except ModuleNotFoundError:
   print("Python Flask library not available.  Run: pip install flask", file=sys.stderr)
   sys.exit(1)

_MAP_DIR = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(_MAP_DIR)
sys.path.insert(0, os.path.join(REPO_ROOT, "parser"))   # upstream parser submodule
sys.path.insert(0, os.path.join(REPO_ROOT, "patches"))  # local fixes not yet merged upstream

import sav_parse
import sav_map_data

STATIC_DIR       = os.path.join(_MAP_DIR, "static", "map")
MAP_IMAGE_FILE   = os.path.join(REPO_ROOT, "map_highres.png")
SFTP_CONFIG_FILE = os.path.join(REPO_ROOT, "sftp_config.json")
SERVER_CONFIG_FILE = os.path.join(REPO_ROOT, "server_config.json")
UPLOADS_DIR      = os.path.join(_MAP_DIR, "uploads")

app = flask.Flask(__name__, static_folder=STATIC_DIR, static_url_path="")

mapDataCache: dict[tuple, dict]  = {}
saveIndexCache: dict[tuple, dict] = {}
mapDataLock = threading.Lock()

loadProgress = {"phase": None, "current": 0, "total": 1}

class _ProgressBarHook(sav_parse.ProgressBar):
   def show(self):
      loadProgress["phase"] = self.prefix.strip().rstrip(":")
      loadProgress["current"] = self.current
      loadProgress["total"] = self.total
      super().show()

sav_parse.ProgressBar = _ProgressBarHook

# ---------------------------------------------------------------------------
# Mode state  (set once by the landing page, persisted across server restarts)
# ---------------------------------------------------------------------------

# type: None | "upload" | "local" | "sftp"
currentMode: dict = {"type": None}

def _saveServerConfig() -> None:
   if currentMode["type"] in ("local", "sftp"):
      try:
         with open(SERVER_CONFIG_FILE, "w") as f:
            json.dump(currentMode, f, indent=2)
      except Exception as e:
         print(f"Warning: could not save server config: {e}", file=sys.stderr)

def _clearServerConfig() -> None:
   try:
      if os.path.isfile(SERVER_CONFIG_FILE):
         os.remove(SERVER_CONFIG_FILE)
   except Exception:
      pass

# ---------------------------------------------------------------------------
# SFTP sync
# ---------------------------------------------------------------------------

sftpSyncStatus: dict = {"lastSync": None, "lastError": None, "syncing": False}
_sftpStopEvent: threading.Event | None = None

def _sftpLocalDir(config: dict) -> str:
   localDir = config.get("local_dir", "sftp_saves")
   return localDir if os.path.isabs(localDir) else os.path.join(REPO_ROOT, localDir)

def sftpSync(config: dict) -> None:
   if sftpSyncStatus["syncing"]:
      return
   try:
      import paramiko
   except ModuleNotFoundError:
      sftpSyncStatus["lastError"] = "paramiko not installed — run: pip install paramiko"
      print(sftpSyncStatus["lastError"], file=sys.stderr)
      return

   sftpSyncStatus["syncing"] = True
   sftpSyncStatus["lastError"] = None
   localDir = _sftpLocalDir(config)
   os.makedirs(localDir, exist_ok=True)
   try:
      ssh = paramiko.SSHClient()
      ssh.set_missing_host_key_policy(paramiko.AutoAddPolicy())
      kwargs: dict = {
         "hostname": config["host"],
         "port":     int(config.get("port", 22)),
         "username": config["username"],
         "timeout":  15,
      }
      if config.get("key_file"):
         kwargs["key_filename"] = os.path.expanduser(config["key_file"])
      elif config.get("password"):
         kwargs["password"] = config["password"]
      ssh.connect(**kwargs)
      sftp = ssh.open_sftp()
      remotePath = config["remote_path"].rstrip("/")
      try:
         entries = sftp.listdir_attr(remotePath)
      except IOError as e:
         raise RuntimeError(f"Cannot list remote path {remotePath!r}: {e}") from e
      downloaded = 0
      for attr in entries:
         if not attr.filename.endswith(".sav"):
            continue
         localPath = os.path.join(localDir, attr.filename)
         if os.path.isfile(localPath):
            ls = os.stat(localPath)
            if ls.st_size == attr.st_size and abs(ls.st_mtime - (attr.st_mtime or 0)) < 2:
               continue
         print(f"SFTP: Downloading {attr.filename}...")
         sftp.get(remotePath + "/" + attr.filename, localPath)
         if attr.st_mtime:
            os.utime(localPath, (attr.st_mtime, attr.st_mtime))
         downloaded += 1
      sftp.close()
      ssh.close()
      sftpSyncStatus["lastSync"] = time.time()
      if downloaded:
         print(f"SFTP: Synced {downloaded} file(s) to {localDir}")
   except Exception as e:
      sftpSyncStatus["lastError"] = str(e)
      print(f"SFTP sync error: {e}", file=sys.stderr)
   finally:
      sftpSyncStatus["syncing"] = False

def _sftpAutoSyncLoop(config: dict, stopEvent: threading.Event) -> None:
   interval = max(10, int(config.get("sync_interval_seconds", 60)))
   while not stopEvent.is_set():
      sftpSync(config)
      stopEvent.wait(interval)

def _startSftpLoop(config: dict) -> None:
   global _sftpStopEvent
   if _sftpStopEvent is not None:
      _sftpStopEvent.set()
   _sftpStopEvent = threading.Event()
   t = threading.Thread(target=_sftpAutoSyncLoop, args=(config, _sftpStopEvent), daemon=True)
   t.start()

# ---------------------------------------------------------------------------
# Mode helpers
# ---------------------------------------------------------------------------

def findDefaultSaveDir() -> str | None:
   if os.path.isdir(".config/Epic/FactoryGame/Saved/SaveGames/server"):
      return os.path.abspath(".config/Epic/FactoryGame/Saved/SaveGames/server")
   if "LOCALAPPDATA" in os.environ:
      d = os.path.join(os.environ["LOCALAPPDATA"], "FactoryGame", "Saved", "SaveGames")
      if os.path.isdir(d):
         return d
   return None

def _applyLocalMode(path: str) -> None:
   app.config["SAVE_DIR"] = path
   app.config["AUTO_LOAD_LATEST"] = True
   app.config.pop("SFTP_CONFIG", None)
   if _sftpStopEvent is not None:
      _sftpStopEvent.set()
   currentMode.clear()
   currentMode.update({"type": "local", "path": path})
   _saveServerConfig()

def _applySftpMode(config: dict) -> None:
   localDir = _sftpLocalDir(config)
   os.makedirs(localDir, exist_ok=True)
   app.config["SAVE_DIR"] = localDir
   app.config["AUTO_LOAD_LATEST"] = True
   app.config["SFTP_CONFIG"] = config
   currentMode.clear()
   currentMode.update({"type": "sftp"})
   _saveServerConfig()
   _startSftpLoop(config)
   print(f"SFTP: syncing {config['host']}:{config['remote_path']} → {localDir} "
         f"(every {config.get('sync_interval_seconds', 60)}s)")

def _applyUploadMode() -> None:
   os.makedirs(UPLOADS_DIR, exist_ok=True)
   app.config["SAVE_DIR"] = UPLOADS_DIR
   app.config["AUTO_LOAD_LATEST"] = True
   app.config.pop("SFTP_CONFIG", None)
   if _sftpStopEvent is not None:
      _sftpStopEvent.set()
   currentMode.clear()
   currentMode.update({"type": "upload"})
   # Upload mode is ephemeral — not written to server_config.json

def _restoreMode() -> None:
   """Called once at startup: re-applies the previously configured mode."""
   if not os.path.isfile(SERVER_CONFIG_FILE):
      return
   try:
      with open(SERVER_CONFIG_FILE, "r") as f:
         cfg = json.load(f)
      modeType = cfg.get("type")
      if modeType == "local":
         path = cfg.get("path", "")
         if os.path.isdir(path):
            _applyLocalMode(path)
            print(f"Restored local mode: {path}")
         else:
            print(f"Warning: saved local path {path!r} no longer exists; showing landing page.")
            _clearServerConfig()
      elif modeType == "sftp":
         sftpCfg = _loadSftpConfig()
         if sftpCfg:
            _applySftpMode(sftpCfg)
            print("Restored SFTP mode.")
         else:
            print("Warning: server_config.json says SFTP but sftp_config.json missing; showing landing page.")
            _clearServerConfig()
   except Exception as e:
      print(f"Warning: could not restore server config: {e}", file=sys.stderr)

# ---------------------------------------------------------------------------
# SFTP config file helpers
# ---------------------------------------------------------------------------

def _loadSftpConfig() -> dict | None:
   if not os.path.isfile(SFTP_CONFIG_FILE):
      return None
   with open(SFTP_CONFIG_FILE, "r") as f:
      return json.load(f)

def _saveSftpConfig(config: dict) -> None:
   with open(SFTP_CONFIG_FILE, "w") as f:
      json.dump(config, f, indent=2)

# ---------------------------------------------------------------------------
# Save listing
# ---------------------------------------------------------------------------

def listSaveFiles(saveDir: str) -> list:
   saveFilenames = set(glob.glob(f"{saveDir}/*.sav")) | set(glob.glob(f"{saveDir}/*/*.sav"))
   saveFiles = [{"filename": os.path.abspath(fn), "displayName": os.path.basename(fn),
                 "mtime": os.path.getmtime(fn)} for fn in saveFilenames]
   saveFiles.sort(key=lambda e: e["mtime"], reverse=True)
   return saveFiles

# ---------------------------------------------------------------------------
# Routes
# ---------------------------------------------------------------------------

@app.route("/")
def landing():
   if currentMode["type"] is not None:
      return flask.redirect("/map")
   return flask.send_from_directory(STATIC_DIR, "landing.html")

@app.route("/map")
def mapView():
   if currentMode["type"] is None:
      return flask.redirect("/")
   return flask.send_from_directory(STATIC_DIR, "index.html")

@app.route("/api/mode")
def apiMode():
   result: dict = {
      "type": currentMode.get("type"),
      "defaultLocalPath": findDefaultSaveDir() or "",
   }
   if currentMode.get("type") == "local":
      result["path"] = currentMode.get("path", "")
   elif currentMode.get("type") == "sftp":
      cfg = app.config.get("SFTP_CONFIG", {})
      result.update({
         "host":         cfg.get("host", ""),
         "port":         cfg.get("port", 22),
         "username":     cfg.get("username", ""),
         "remotePath":   cfg.get("remote_path", ""),
         "syncInterval": cfg.get("sync_interval_seconds", 60),
      })
   else:
      # Pre-fill SFTP fields from sftp_config.json if it exists (even when no
      # active mode yet) so the user doesn't have to re-type credentials.
      cfg = _loadSftpConfig() or {}
      if cfg:
         result.update({
            "sftpPrefill": True,
            "host":         cfg.get("host", ""),
            "port":         cfg.get("port", 22),
            "username":     cfg.get("username", ""),
            "remotePath":   cfg.get("remote_path", ""),
            "syncInterval": cfg.get("sync_interval_seconds", 60),
         })
   return flask.jsonify(result)

@app.route("/api/set-mode", methods=["POST"])
def apiSetMode():
   data = flask.request.get_json(force=True) or {}
   mode = data.get("mode")

   if mode == "local":
      path = data.get("path", "").strip()
      if not path or not os.path.isdir(path):
         return flask.jsonify({"error": f"Folder not found: {path!r}"}), 400
      _applyLocalMode(path)
      return flask.jsonify({"ok": True})

   if mode == "sftp":
      host = data.get("host", "").strip()
      username = data.get("username", "").strip()
      remotePath = data.get("remotePath", "").strip()
      if not host or not username or not remotePath:
         return flask.jsonify({"error": "Host, username and remote path are required."}), 400
      config = {
         "host":                  host,
         "port":                  int(data.get("port", 22)),
         "username":              username,
         "password":              data.get("password", ""),
         "key_file":              data.get("keyFile", ""),
         "remote_path":           remotePath,
         "local_dir":             "sftp_saves",
         "sync_interval_seconds": int(data.get("syncInterval", 60)),
      }
      _saveSftpConfig(config)
      _applySftpMode(config)
      return flask.jsonify({"ok": True})

   return flask.jsonify({"error": f"Unknown mode: {mode!r}"}), 400

@app.route("/api/upload-save", methods=["POST"])
def apiUploadSave():
   if "file" not in flask.request.files:
      return flask.jsonify({"error": "No file provided."}), 400
   f = flask.request.files["file"]
   if not f.filename or not f.filename.endswith(".sav"):
      return flask.jsonify({"error": "File must be a .sav save file."}), 400
   os.makedirs(UPLOADS_DIR, exist_ok=True)
   dest = os.path.join(UPLOADS_DIR, os.path.basename(f.filename))
   f.save(dest)
   _applyUploadMode()
   return flask.jsonify({"ok": True})

@app.route("/api/reset-mode", methods=["POST"])
def apiResetMode():
   currentMode.clear()
   currentMode["type"] = None
   app.config.pop("SAVE_DIR", None)
   app.config.pop("AUTO_LOAD_LATEST", None)
   app.config.pop("SFTP_CONFIG", None)
   if _sftpStopEvent is not None:
      _sftpStopEvent.set()
   _clearServerConfig()
   return flask.jsonify({"ok": True})

@app.route("/api/saves")
def apiSaves():
   saveDir = flask.request.args.get("dir") or app.config.get("SAVE_DIR")
   if not saveDir:
      return flask.jsonify({"error": "No save directory configured."}), 400
   return flask.jsonify(listSaveFiles(saveDir))

@app.route("/api/config")
def apiConfig():
   return flask.jsonify({
      "autoLoadLatest": app.config.get("AUTO_LOAD_LATEST", False),
      "sftpEnabled":    "SFTP_CONFIG" in app.config,
   })

@app.route("/api/sftp-status")
def apiSftpStatus():
   if "SFTP_CONFIG" not in app.config:
      return flask.jsonify({"enabled": False})
   return flask.jsonify({
      "enabled":   True,
      "syncing":   sftpSyncStatus["syncing"],
      "lastSync":  sftpSyncStatus["lastSync"],
      "lastError": sftpSyncStatus["lastError"],
   })

@app.route("/api/sftp-sync", methods=["POST"])
def apiSftpSync():
   config = app.config.get("SFTP_CONFIG")
   if not config:
      return flask.jsonify({"error": "SFTP not configured."}), 400
   if sftpSyncStatus["syncing"]:
      return flask.jsonify({"status": "already_syncing"})
   threading.Thread(target=sftpSync, args=(config,), daemon=True).start()
   return flask.jsonify({"status": "started"})

@app.route("/api/map-data")
def apiMapData():
   filename = flask.request.args.get("file")
   if not filename or not os.path.isfile(filename):
      return flask.jsonify({"error": f"Save file not found: {filename}"}), 404
   cacheKey = (filename, os.path.getmtime(filename))
   with mapDataLock:
      if cacheKey not in mapDataCache:
         loadProgress.update({"phase": "Starting", "current": 0, "total": 1})
         try:
            parsedSave = sav_parse.readFullSaveFile(filename)
         except sav_parse.ParseError as error:
            loadProgress.update({"phase": None, "current": 0, "total": 1})
            return flask.jsonify({"error": str(error)}), 500
         loadProgress.update({"phase": "Building map data", "current": 0, "total": 1})
         newMapData  = sav_map_data.buildMapPayload(parsedSave)
         newSaveIdx  = sav_map_data.buildSaveIndex(parsedSave)
         mapDataCache.clear()
         saveIndexCache.clear()
         mapDataCache[cacheKey]  = newMapData
         saveIndexCache[cacheKey] = newSaveIdx
         loadProgress.update({"phase": None, "current": 1, "total": 1})
   return flask.jsonify(mapDataCache[cacheKey])

@app.route("/api/load-progress")
def apiLoadProgress():
   return flask.jsonify(loadProgress)

@app.route("/api/instance")
def apiInstance():
   filename     = flask.request.args.get("file")
   instanceName = flask.request.args.get("instance")
   if not filename or not os.path.isfile(filename) or not instanceName:
      return flask.jsonify({"error": "Missing or invalid file/instance parameter."}), 400
   cacheKey  = (filename, os.path.getmtime(filename))
   saveIndex = saveIndexCache.get(cacheKey)
   if saveIndex is None:
      return flask.jsonify({"error": "This save isn't currently loaded. Click Load again, then retry."}), 409
   return flask.jsonify(sav_map_data.describeInstance(saveIndex, instanceName))

@app.route("/map_highres.png")
def mapImage():
   return flask.send_from_directory(REPO_ROOT, "map_highres.png")

# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main():
   parser = argparse.ArgumentParser(description="Satisfactory save-file map viewer server.")
   parser.add_argument("--port",       type=int, default=5000)
   parser.add_argument("--host",       default="127.0.0.1")
   parser.add_argument("--no-browser", action="store_true")
   args = parser.parse_args()

   if not os.path.isfile(MAP_IMAGE_FILE):
      print(f"ERROR: map_highres.png not found in {REPO_ROOT}.", file=sys.stderr)
      sys.exit(1)

   _restoreMode()

   url = f"http://{args.host}:{args.port}/"
   print(f"Map viewer: {url}")
   if currentMode["type"]:
      print(f"Mode restored: {currentMode['type']}")
   else:
      print("No mode configured — landing page will be shown.")
   if not args.no_browser:
      webbrowser.open(url)
   app.run(host=args.host, port=args.port, debug=False, threaded=True)

if __name__ == "__main__":
   main()
