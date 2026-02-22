#!/bin/bash

set -e

# Build the release version of the program
cargo build --release

# Copy built binary to /usr/bin (sudo if needed)
sudo cp ./target/release/pcipoke /usr/bin/