#!/bin/bash
# Test rename operations: basic rename, RENAME_NOREPLACE, RENAME_EXCHANGE,
# cross-layer renames, rename with whiteout creation.

set -xeuo pipefail

# Helper: create a C program for renameat2 with flags
compile_renameat2_helper() {
    cat > rename_helper.c << 'CEOF'
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <errno.h>
#include <string.h>
#include <linux/fs.h>

#ifndef RENAME_NOREPLACE
#define RENAME_NOREPLACE (1 << 0)
#endif
#ifndef RENAME_EXCHANGE
#define RENAME_EXCHANGE (1 << 1)
#endif

int main(int argc, char *argv[]) {
    if (argc != 4) {
        fprintf(stderr, "Usage: %s <src> <dst> <noreplace|exchange>\n", argv[0]);
        return 1;
    }
    unsigned int flags = 0;
    if (strcmp(argv[3], "noreplace") == 0)
        flags = RENAME_NOREPLACE;
    else if (strcmp(argv[3], "exchange") == 0)
        flags = RENAME_EXCHANGE;
    else {
        fprintf(stderr, "Unknown flag: %s\n", argv[3]);
        return 1;
    }

    int ret = syscall(SYS_renameat2, AT_FDCWD, argv[1], AT_FDCWD, argv[2], flags);
    if (ret < 0) {
        fprintf(stderr, "renameat2(%s, %s, %s): %s (errno=%d)\n",
                argv[1], argv[2], argv[3], strerror(errno), errno);
        return errno;
    }
    return 0;
}
CEOF
    gcc -o rename_helper rename_helper.c
}

cleanup() {
    cd /
    umount "$MERGED" 2>/dev/null || true
    rm -rf "$TESTDIR"
}

TESTDIR=$(mktemp -d /tmp/test-rename.XXXXXX)
trap cleanup EXIT
cd "$TESTDIR"

MERGED="$TESTDIR/merged"

compile_renameat2_helper

# ========================================
# Test 1: Basic rename within upper layer
# ========================================
echo "=== Test 1: Basic rename within upper layer ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "hello" > merged/file1
mv merged/file1 merged/file2
test -f merged/file2
test ! -e merged/file1
grep hello merged/file2

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 2: Rename from lower layer creates whiteout
# ========================================
echo "=== Test 2: Rename from lower creates whiteout ==="
mkdir -p lower upper workdir merged

echo "lower_content" > lower/orig
fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

mv merged/orig merged/renamed
test -f merged/renamed
test ! -e merged/orig
grep lower_content merged/renamed

# Verify whiteout was created in upper
if [ -c upper/orig ]; then
    # char device whiteout
    test "$(stat -c '%t:%T' upper/orig)" = "0:0"
elif [ -f upper/.wh.orig ]; then
    # .wh. file whiteout
    true
else
    echo "ERROR: No whiteout found for orig"
    exit 1
fi

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 3: Rename directory from lower layer
# ========================================
echo "=== Test 3: Rename directory from lower layer ==="
mkdir -p lower/dir1 upper workdir merged
echo "file_in_dir" > lower/dir1/file

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

mv merged/dir1 merged/dir2
test -d merged/dir2
test ! -e merged/dir1
grep file_in_dir merged/dir2/file

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 4: Rename overwrites existing file
# ========================================
echo "=== Test 4: Rename overwrites existing file ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "src" > merged/src
echo "dst" > merged/dst
mv merged/src merged/dst
test ! -e merged/src
grep src merged/dst

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 5: RENAME_NOREPLACE - should fail if dest exists
# ========================================
echo "=== Test 5: RENAME_NOREPLACE ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "src" > merged/src5
echo "dst" > merged/dst5

# RENAME_NOREPLACE should fail with EEXIST (errno 17)
if ./rename_helper merged/src5 merged/dst5 noreplace; then
    echo "ERROR: RENAME_NOREPLACE should have failed"
    exit 1
fi
# Both files should still exist
test -f merged/src5
test -f merged/dst5
grep src merged/src5
grep dst merged/dst5

# RENAME_NOREPLACE should succeed if dest doesn't exist
./rename_helper merged/src5 merged/new5 noreplace
test ! -e merged/src5
test -f merged/new5
grep src merged/new5

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 6: RENAME_EXCHANGE - atomic swap
# ========================================
echo "=== Test 6: RENAME_EXCHANGE ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "aaa" > merged/fileA
echo "bbb" > merged/fileB
./rename_helper merged/fileA merged/fileB exchange
grep bbb merged/fileA
grep aaa merged/fileB

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 7: RENAME_EXCHANGE between directories
# ========================================
echo "=== Test 7: RENAME_EXCHANGE directories ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

mkdir merged/dirA merged/dirB
echo "inA" > merged/dirA/fa
echo "inB" > merged/dirB/fb
./rename_helper merged/dirA merged/dirB exchange
test -f merged/dirA/fb
test -f merged/dirB/fa

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 8: Rename file over lower-layer file
# ========================================
echo "=== Test 8: Rename over lower-layer file ==="
mkdir -p lower upper workdir merged

echo "lower_dst" > lower/dst8
fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "upper_src" > merged/src8
mv merged/src8 merged/dst8
grep upper_src merged/dst8
test ! -e merged/src8

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 9: Cross-directory rename
# ========================================
echo "=== Test 9: Cross-directory rename ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

mkdir -p merged/dir_src merged/dir_dst
echo "cross" > merged/dir_src/file
mv merged/dir_src/file merged/dir_dst/file
test ! -e merged/dir_src/file
grep cross merged/dir_dst/file

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 10: Rename lower-layer file then re-create
# ========================================
echo "=== Test 10: Rename then re-create ==="
mkdir -p lower upper workdir merged

echo "original" > lower/recreate
fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

mv merged/recreate merged/moved
echo "new_content" > merged/recreate
grep original merged/moved
grep new_content merged/recreate

umount merged
rm -rf lower upper workdir merged

echo "All rename tests passed!"
