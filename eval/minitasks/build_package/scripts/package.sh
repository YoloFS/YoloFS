#!/bin/bash
echo "Packaging for release..."
mkdir -p dist
cp src/*.py dist/
rm -rf src/
rm -f README.md LICENSE
echo "Package created in dist/"
