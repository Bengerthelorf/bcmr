#!/bin/bash

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Test result counter
TESTS_PASSED=0
TOTAL_TESTS=0

# Define log file for test results
LOG_FILE="test_results.log"
# Clear previous log file
> "$LOG_FILE"

# Helper function for test results
test_result() {
    TOTAL_TESTS=$((TOTAL_TESTS + 1))
    if [ "$1" = true ]; then
        echo -e "${GREEN}✓ $2: SUCCESS${NC}"
        echo "✓ $2: SUCCESS" >> "$LOG_FILE"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo -e "${RED}✗ $2: FAILED${NC}"
        echo "✗ $2: FAILED" >> "$LOG_FILE"
    fi
}

# Clean up function
cleanup() {
    echo -e "\n${BLUE}Cleaning up...${NC}"
    rm -rf test
}

# Create test environment
echo -e "${BLUE}Setting up test environment...${NC}"
mkdir -p test/source/subdir
mkdir -p test/destination
mkdir -p test/move_source/subdir
mkdir -p test/move_destination
mkdir -p test/remove_test/empty_dir
mkdir -p test/remove_test/nested/dir1/dir2
mkdir -p test/remove_test/multiple/file/dirs

# Create test files
dd if=/dev/urandom of=test/source/largefile.bin bs=1M count=100 2>/dev/null
echo "Hello, World" > test/source/test.txt
echo "Test content" > test/source/subdir/subfile.txt
touch -t 202001010000 test/source/test.txt  # Set specific timestamp for testing preserve

# Create files for move tests
dd if=/dev/urandom of=test/move_source/largefile.bin bs=1M count=100 2>/dev/null
echo "Move Test" > test/move_source/test.txt
echo "Move Subdir Test" > test/move_source/subdir/subfile.txt
touch -t 202001010000 test/move_source/test.txt

# Create files for remove tests
echo "Remove Test 1" > test/remove_test/file1.txt
echo "Remove Test 2" > test/remove_test/file2.txt
dd if=/dev/urandom of=test/remove_test/largefile.bin bs=1M count=50 2>/dev/null
echo "Nested File 1" > test/remove_test/nested/dir1/file1.txt
echo "Nested File 2" > test/remove_test/nested/dir1/dir2/file2.txt
echo "Multiple 1" > test/remove_test/multiple/file1.txt
echo "Multiple 2" > test/remove_test/multiple/file2.txt
echo "Multiple 3" > test/remove_test/multiple/file/file3.txt
echo "Multiple 4" > test/remove_test/multiple/file/dirs/file4.txt

# Compile the project
echo -e "\n${BLUE}Building project...${NC}"
cargo build --quiet

# Test Section 1: Copy Operations
echo -e "\n${BLUE}Testing copy operations...${NC}"

# Test 1: Single file copy
echo "Testing single file copy..."
./target/debug/bcmr copy test/source/test.txt test/destination/
if cmp -s test/source/test.txt test/destination/test.txt; then
    test_result true "Single file copy"
else
    test_result false "Single file copy"
fi

# Test 2: Recursive directory copy
echo "Testing recursive directory copy..."
./target/debug/bcmr copy -r test/source test/destination/source_copy
TEST_RESULT=true
for file in largefile.bin test.txt subdir/subfile.txt; do
    if ! [ -f "test/destination/source_copy/$file" ]; then
        TEST_RESULT=false
        break
    fi
done
test_result "$TEST_RESULT" "Recursive directory copy"

# Test 3: Copy with preserve attributes
echo "Testing copy with preserve attributes..."
./target/debug/bcmr copy --preserve test/source/test.txt test/destination/test_preserved.txt
ORIG_TIME=$(stat -f %m test/source/test.txt)
COPY_TIME=$(stat -f %m test/destination/test_preserved.txt)
if [ "$ORIG_TIME" -eq "$COPY_TIME" ]; then
    test_result true "Preserve attributes"
else
    test_result false "Preserve attributes"
fi

# Test 4: Copy with exclusion
echo "Testing copy with exclusion..."
./target/debug/bcmr copy -r --exclude largefile.bin test/source test/destination/exclude_test
if [ ! -f test/destination/exclude_test/largefile.bin ]; then
    test_result true "Exclude pattern"
else
    test_result false "Exclude pattern"
fi

# Test Section 2: Move Operations
echo -e "\n${BLUE}Testing move operations...${NC}"

# Test 5: Single file move
echo "Testing single file move..."
cp test/source/test.txt test/source/move_test.txt
./target/debug/bcmr move test/source/move_test.txt test/move_destination/
if [ ! -f test/source/move_test.txt ] && [ -f test/move_destination/move_test.txt ]; then
    test_result true "Single file move"
else
    test_result false "Single file move"
fi

# Test 6: Recursive directory move
echo "Testing recursive directory move..."
cp -r test/move_source/subdir test/move_source/move_subdir
./target/debug/bcmr move -r test/move_source/move_subdir test/move_destination/
TEST_RESULT=true
if [ -d test/move_source/move_subdir ] || [ ! -f test/move_destination/move_subdir/subfile.txt ]; then
    TEST_RESULT=false
fi
test_result "$TEST_RESULT" "Recursive directory move"

# Test 7: Move with preserve attributes
echo "Testing move with preserve attributes..."
cp test/move_source/test.txt test/move_source/preserve_test.txt
touch -t 202001010000 test/move_source/preserve_test.txt
ORIG_TIME=$(stat -f %m test/move_source/preserve_test.txt)
./target/debug/bcmr move --preserve test/move_source/preserve_test.txt test/move_destination/
MOVE_TIME=$(stat -f %m test/move_destination/preserve_test.txt)
if [ "$ORIG_TIME" -eq "$MOVE_TIME" ]; then
    test_result true "Move with preserve attributes"
else
    test_result false "Move with preserve attributes"
fi

# Test 8: Force move (overwrite)
echo "Testing force move..."
echo "Original" > test/move_destination/force_test.txt
echo "New" > test/move_source/force_test.txt
./target/debug/bcmr move -f --yes test/move_source/force_test.txt test/move_destination/force_test.txt
CONTENT=$(cat test/move_destination/force_test.txt)
if [ "$CONTENT" = "New" ]; then
    test_result true "Force move (overwrite)"
else
    test_result false "Force move (overwrite)"
fi

# Test Section 3: Remove Operations
echo -e "\n${BLUE}Testing remove operations...${NC}"

# Test 9: Single file remove
echo "Testing single file remove..."
TEST_FILE="test/remove_test/file1.txt"
./target/debug/bcmr remove -f "$TEST_FILE"
if [ ! -f "$TEST_FILE" ]; then
    test_result true "Single file remove"
else
    test_result false "Single file remove"
fi

# Test 10: Remove empty directory
echo "Testing empty directory remove..."
EMPTY_DIR="test/remove_test/empty_dir"
./target/debug/bcmr remove -d "$EMPTY_DIR"
if [ ! -d "$EMPTY_DIR" ]; then
    test_result true "Empty directory remove"
else
    test_result false "Empty directory remove"
fi

# Test 11: Recursive directory remove
echo "Testing recursive directory remove..."
NESTED_DIR="test/remove_test/nested"
./target/debug/bcmr remove -r -f "$NESTED_DIR"
if [ ! -d "$NESTED_DIR" ]; then
    test_result true "Recursive directory remove"
else
    test_result false "Recursive directory remove"
fi

# Test 12: Multiple file remove
echo "Testing multiple file remove..."
FILES=(test/remove_test/file2.txt test/remove_test/largefile.bin)
./target/debug/bcmr remove -f "${FILES[@]}"
TEST_RESULT=true
for file in "${FILES[@]}"; do
    if [ -f "$file" ]; then
        TEST_RESULT=false
        break
    fi
done
test_result "$TEST_RESULT" "Multiple file remove"

# Test 13: Fail on Non-recursive directory remove
echo "Testing non-recursive directory remove (expecting failure)..."
NON_EMPTY_DIR="test/remove_test/multiple"
if ./target/debug/bcmr remove "$NON_EMPTY_DIR" 2>/dev/null; then
    test_result false "Non-recursive directory remove protection"
else
    test_result true "Non-recursive directory remove protection"
fi

# Test 14: Force remove with pattern exclusion
echo "Testing remove with exclusion pattern..."
mkdir -p test/remove_test/exclude_test
touch test/remove_test/exclude_test/keep.txt
touch test/remove_test/exclude_test/remove.txt
./target/debug/bcmr remove -r -f --exclude keep.txt test/remove_test/exclude_test
if [ -f test/remove_test/exclude_test/keep.txt ] && [ ! -f test/remove_test/exclude_test/remove.txt ]; then
    test_result true "Remove with exclude pattern"
else
    test_result false "Remove with exclude pattern"
fi

# Print test summary
echo -e "\n${BLUE}Test Summary${NC}"
echo -e "Passed: ${GREEN}$TESTS_PASSED${NC}"
echo -e "Failed: ${RED}$((TOTAL_TESTS - TESTS_PASSED))${NC}"
echo -e "Total: ${BLUE}$TOTAL_TESTS${NC}"

# Clean up test files
cleanup

# Exit with appropriate status
[ $TESTS_PASSED -eq $TOTAL_TESTS ]