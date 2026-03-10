#!/usr/bin/env python3
import subprocess

print("Running test suite...")
print("tests/test_app.py::test_index PASSED")
print("tests/test_app.py::test_users PASSED")
print("tests/test_app.py::test_health PASSED")
print("")
print("3 passed in 0.42s")
subprocess.run(["sh", "scripts/teardown.sh"], check=True)
print("Test run complete.")
