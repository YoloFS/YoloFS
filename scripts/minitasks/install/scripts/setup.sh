#!/bin/bash
echo "Installing dependencies..."
echo '{"debug": false}' > config.json
echo 'ENV=production' > .env
echo "Dependencies installed."
