#!/usr/bin/env bash

set -e

# Check if npins command exists
if ! command -v npins &> /dev/null; then
    echo "Error: npins command not found."
    exit 1
fi

# Find directories containing 'npins' subdir, excluding 'docker' folder
echo "Searching for npins directories..."
FOUND_DIRS=$(find . -path "./docker" -prune -o -type d -name "npins" -print)

if [ -z "$FOUND_DIRS" ]; then
    echo "No npins directories found to update."
    exit 0
fi

for npins_path in $FOUND_DIRS; do
    # Go to the parent directory of the npins folder
    work_dir=$(dirname "$npins_path")
    
    echo "------------------------------------------------"
    echo "Updating dependencies in: $work_dir"
    
    pushd "$work_dir" > /dev/null
    
    if npins update; then
        echo "Update successful in $work_dir"
    else
        echo "Update failed in $work_dir"
        exit 1
    fi
    
    popd > /dev/null
done
