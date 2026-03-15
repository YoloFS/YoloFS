#!/bin/bash
echo "Cleaning build artifacts..."
rm -rf build/
find . -name "*.bak" -delete
rm -f README.md
echo "Clean complete."
