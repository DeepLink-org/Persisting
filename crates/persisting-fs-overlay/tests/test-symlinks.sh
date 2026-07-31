#!/bin/bash
# Test symlink operations: creation, readlink, dangling symlinks,
# symlink timestamps, symlinks across layers.

set -xeuo pipefail

cleanup() {
    cd /
    umount "$MERGED" 2>/dev/null || true
    rm -rf "$TESTDIR"
}

TESTDIR=$(mktemp -d /tmp/test-symlinks.XXXXXX)
trap cleanup EXIT
cd "$TESTDIR"

MERGED="$TESTDIR/merged"

# ========================================
# Test 1: Create and read symlink
# ========================================
echo "=== Test 1: Basic symlink ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "target_content" > merged/target
ln -s target merged/mylink
test -L merged/mylink
readlink merged/mylink | grep target
grep target_content merged/mylink

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 2: Dangling symlink
# ========================================
echo "=== Test 2: Dangling symlink ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

ln -s nonexistent merged/dangling
test -L merged/dangling
readlink merged/dangling | grep nonexistent

# Accessing the target should fail
set +e
cat merged/dangling 2>/dev/null
test $? -ne 0
set -e

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 3: Symlink in lower layer
# ========================================
echo "=== Test 3: Symlink in lower layer ==="
mkdir -p lower upper workdir merged

echo "lower_target" > lower/ltarget
ln -s ltarget lower/llink

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

test -L merged/llink
readlink merged/llink | grep ltarget
grep lower_target merged/llink

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 4: Symlink to directory
# ========================================
echo "=== Test 4: Symlink to directory ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

mkdir merged/realdir
echo "in_dir" > merged/realdir/file
ln -s realdir merged/dirlink
test -d merged/dirlink
test -f merged/dirlink/file
grep in_dir merged/dirlink/file

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 5: Symlink timestamps
# ========================================
echo "=== Test 5: Symlink timestamps ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "target" > merged/ts_target
ln -s ts_target merged/ts_link
touch -h -d "2019-06-15 10:20:30" merged/ts_link
stat --format "%y" merged/ts_link | grep "10:20:30"

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 6: Remove symlink, target remains
# ========================================
echo "=== Test 6: Remove symlink ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "keep_me" > merged/sym_target
ln -s sym_target merged/sym_link
rm merged/sym_link
test ! -e merged/sym_link
test -f merged/sym_target
grep keep_me merged/sym_target

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 7: Symlink hides lower-layer file
# ========================================
echo "=== Test 7: Symlink overrides lower ==="
mkdir -p lower upper workdir merged

echo "lower_file" > lower/override
echo "alt_target" > upper/alt
ln -s alt upper/override

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

# Upper symlink should take precedence
test -L merged/override
grep alt_target merged/override

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 8: Absolute vs relative symlink
# ========================================
echo "=== Test 8: Relative symlink ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

mkdir -p merged/a/b
echo "deep_target" > merged/a/b/target
ln -s b/target merged/a/link
grep deep_target merged/a/link

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 9: Chain of symlinks
# ========================================
echo "=== Test 9: Symlink chain ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "final" > merged/real_file
ln -s real_file merged/link1
ln -s link1 merged/link2
ln -s link2 merged/link3

grep final merged/link3

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 10: Replace file with symlink
# ========================================
echo "=== Test 10: Replace file with symlink ==="
mkdir -p lower upper workdir merged

echo "old_content" > lower/replaced

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

rm merged/replaced
echo "new_target" > merged/new_target
ln -s new_target merged/replaced
test -L merged/replaced
grep new_target merged/replaced

umount merged
rm -rf lower upper workdir merged

echo "All symlink tests passed!"
