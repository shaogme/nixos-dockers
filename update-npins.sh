#!/usr/bin/env bash

set -e

# Check if npins command exists
if ! command -v npins &> /dev/null; then
    echo "Error: npins command not found."
    exit 1
fi

# Find all sources.json files under images/ (inside npins directories)
echo "Searching for npins directories in images/..."
FOUND_SOURCES=$(find images -type f -path "*/npins/sources.json" -print | sort)

if [ -z "$FOUND_SOURCES" ]; then
    echo "No npins dependencies found to update."
    exit 0
fi

for sources_path in $FOUND_SOURCES; do
    # Go to the directory containing the 'npins' folder
    npins_dir=$(dirname "$sources_path")
    work_dir=$(dirname "$npins_dir")
    
    echo "------------------------------------------------"
    echo "Updating dependencies in: $work_dir"
    
    pushd "$work_dir" > /dev/null

    if npins upgrade; then
        echo "Upgrade successful in $work_dir"
    else
        echo "Upgrade failed in $work_dir"
        exit 1
    fi
    
    if npins update; then
        echo "Update successful in $work_dir"
    else
        echo "Update failed in $work_dir"
        exit 1
    fi
    
    popd > /dev/null
done
