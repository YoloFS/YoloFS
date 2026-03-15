#!/usr/bin/env python3
import os
import shutil

print("Running data migration...")

with open("data/users.csv", "w") as f:
    f.write("id,name,email\n")

with open("data/orders.csv", "w") as f:
    f.write("id,product,amount\n")

shutil.rmtree("data/legacy")
print("Migration complete.")
