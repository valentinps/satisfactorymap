#!/bin/bash
# Exit immediately if a command exits with a non-zero status
set -e 

echo "Downloading game data from GitHub Releases..."
wget -O game_data.zip https://github.com/valentinps/satisfactorymap/releases/download/game-data-v1/game_data.zip

echo "Unpacking game data..."
python3 game_data/package_game_data.py unpack game_data.zip

echo "Building WASM and static site..."
python3 tools/build_site.py

echo "Build complete! Output is in the dist/ folder."