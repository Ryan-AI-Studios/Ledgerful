#!/bin/bash
# Pre-commit hook for Ledgerful

echo "Running engineering hygiene checks..."

# 1. Format check
echo "Checking formatting..."
cargo fmt --all -- --check
if [ $? -ne 0 ]; then
    echo "ERROR: Code is not formatted. Run 'cargo fmt --all' and try again."
    exit 1
fi

# 2. Lint check
echo "Running clippy..."
cargo clippy --all-targets --all-features -- -D warnings
if [ $? -ne 0 ]; then
    echo "ERROR: Clippy found warnings/errors. Fix them and try again."
    exit 1
fi

# 3. Test check
echo "Running tests..."
cargo test --workspace -- -j 1 --test-threads=1
if [ $? -ne 0 ]; then
    echo "ERROR: Tests failed. Fix them and try again."
    exit 1
fi

echo "Hygiene checks PASSED."
exit 0
