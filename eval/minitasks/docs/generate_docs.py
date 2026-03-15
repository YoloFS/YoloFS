#!/usr/bin/env python3
print("Generating API documentation...")

with open("docs/api.md", "w") as f:
    f.write("# API Reference\n\nAuto-generated. Do not edit.\n")

with open("docs/guide.md", "w") as f:
    f.write("# User Guide\n\nAuto-generated. Do not edit.\n")

print("Documentation generated successfully.")
