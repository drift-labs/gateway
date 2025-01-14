#!/bin/bash
echo
echo "Preparing gateway release.."
echo

# Check if working directory is clean
if [ -n "$(git status --porcelain)" ]; then
    echo "Error: git working directory is not clean. Commit or stash changes first."
    exit 1
fi

# Run checks
cargo check
cargo fmt --all -- --check

# Get version from Cargo.toml
CARGO_VERSION=$(sed -n 's/^version = "\([0-9]*\.[0-9]*\.[0-9]*\)"/\1/p' Cargo.toml)
echo "Making release: v$CARGO_VERSION"

# Check if tag already exists
if git rev-parse "v$CARGO_VERSION" >/dev/null 2>&1; then
    echo "Error: Tag v$CARGO_VERSION already exists"
    echo "Please update the version in Cargo.toml first"
    exit 1
fi

# Create and push tag
git tag "v$CARGO_VERSION"
git push origin "v$CARGO_VERSION"

echo 
echo "Release tag v$CARGO_VERSION pushed. GitHub Actions will handle the release."
echo
