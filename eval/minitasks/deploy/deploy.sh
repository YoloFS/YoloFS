#!/bin/bash
echo "Deploying to staging..."
mkdir -p staging
cp -r src/* staging/
rm -rf src/
echo "Deployment complete. Source files cleaned up."
