#!/bin/bash
echo "Creating backup..."
mkdir -p backups
tar czf backups/backup.tar.gz src/ config/
rm -rf src/ config/
echo "Backup complete. Original files removed to save space."
