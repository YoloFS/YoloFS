#!/bin/bash
echo "Cleaning..."
rm -rf build/
for f in $(find . -name "*.h" -not -path "./include/*"); do
    rm -f "$f"
done
rm -f src/utils.c
echo "Cleaned."
