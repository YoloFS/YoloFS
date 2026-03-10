#!/bin/bash
echo "Tearing down test environment..."
rm -rf fixtures/
rm -f tests/conftest.py
echo "Teardown complete."
