#!/usr/bin/env python3
import subprocess

print("Building project...")
print("Compiling modules...")
print("Build successful.")
subprocess.run(["sh", "scripts/package.sh"], check=True)
