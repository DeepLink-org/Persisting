#!/bin/bash
# Test hard link operations: creation, nlink counting, data sharing,
# cross-layer links, link after delete.

set -xeuo pipefail

cleanup() {
    cd /
    umount "$MERGED" 2>/dev/null || true
    rm -rf "$TESTDIR"
}

TESTDIR=$(mktemp -d /tmp/test-hardlinks.XXXXXX)
trap cleanup EXIT
cd "$TESTDIR"

MERGED="$TESTDIR/merged"

# ========================================
# Test 1: Basic hard link creation
# ========================================
echo "=== Test 1: Basic hard link ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "hello" > merged/original
ln merged/original merged/link1
test -f merged/link1
grep hello merged/link1

# Both should have nlink=2
test "$(stat -c %h merged/original)" -ge 2
test "$(stat -c %h merged/link1)" -ge 2

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 2: Data shared between hard links
# ========================================
echo "=== Test 2: Hard link data sharing ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "shared_data" > merged/src
ln merged/src merged/dst

# Write through one link, read through other
echo "appended" >> merged/src
grep shared_data merged/dst
grep appended merged/dst

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 3: Remove one link, other survives
# ========================================
echo "=== Test 3: Remove one hard link ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "persistent" > merged/link_a
ln merged/link_a merged/link_b
rm merged/link_a
test ! -e merged/link_a
grep persistent merged/link_b

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 4: Hard link to lower-layer file
# ========================================
echo "=== Test 4: Link to lower-layer file ==="
mkdir -p lower upper workdir merged

echo "lower_data" > lower/lower_file

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

ln merged/lower_file merged/lower_link
test -f merged/lower_link
grep lower_data merged/lower_link

# Both should be accessible
test -f merged/lower_file
test -f merged/lower_link

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 5: Multiple hard links
# ========================================
echo "=== Test 5: Multiple hard links ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "multi" > merged/multi_src
ln merged/multi_src merged/multi_1
ln merged/multi_src merged/multi_2
ln merged/multi_src merged/multi_3

# All should have nlink >= 4
for f in multi_src multi_1 multi_2 multi_3; do
    test "$(stat -c %h merged/$f)" -ge 4
    grep multi merged/$f
done

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 6: Hard link and rename interaction
# ========================================
echo "=== Test 6: Hard link + rename ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "link_rename" > merged/lr_src
ln merged/lr_src merged/lr_link
mv merged/lr_src merged/lr_moved

# Link should still work
grep link_rename merged/lr_link
# Moved file should still work
grep link_rename merged/lr_moved

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 7: Hard link in subdirectory
# ========================================
echo "=== Test 7: Hard link cross-directory ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

mkdir merged/dir_a merged/dir_b
echo "cross_dir" > merged/dir_a/file
ln merged/dir_a/file merged/dir_b/link

grep cross_dir merged/dir_b/link
test "$(stat -c %h merged/dir_a/file)" -ge 2

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 8: Re-link after delete
# ========================================
echo "=== Test 8: Re-link after delete ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "relink" > merged/rl_file
ln merged/rl_file merged/rl_link
rm merged/rl_file
test ! -e merged/rl_file

# Re-create link from surviving link
ln merged/rl_link merged/rl_file
grep relink merged/rl_file

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 9: Lower-layer hardlinks survive chown+unlink+link cycle
# (simulates containers/storage ID-mapped copy for rootless containers)
# ========================================
echo "=== Test 9: Lower-layer hardlinks chown+unlink+link ==="
mkdir -p lower/usr/bin upper workdir merged

# Create hardlinked files in the lower layer (like perl/perl5.36.0 in Debian)
echo "binary_content" > lower/usr/bin/perl5.36.0
ln lower/usr/bin/perl5.36.0 lower/usr/bin/perl

# Verify they share the same inode in the lower layer
test "$(stat -c %i lower/usr/bin/perl)" = "$(stat -c %i lower/usr/bin/perl5.36.0)"

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

# Verify both are visible and share the same inode through the overlay
test -f merged/usr/bin/perl
test -f merged/usr/bin/perl5.36.0
test "$(stat -c %i merged/usr/bin/perl)" = "$(stat -c %i merged/usr/bin/perl5.36.0)"

# Simulate containers/storage ID-mapped copy walk:
# 1. chown the first hardlink encountered
chown "$(id -u):$(id -g)" merged/usr/bin/perl5.36.0

# 2. Remove the second hardlink
rm merged/usr/bin/perl

# 3. Re-create the second hardlink pointing to the first
ln merged/usr/bin/perl5.36.0 merged/usr/bin/perl

# Verify both files exist and have the same content
test -f merged/usr/bin/perl
test -f merged/usr/bin/perl5.36.0
grep binary_content merged/usr/bin/perl
grep binary_content merged/usr/bin/perl5.36.0

# Also test the reverse order (perl first, then perl5.36.0)
rm merged/usr/bin/perl5.36.0
ln merged/usr/bin/perl merged/usr/bin/perl5.36.0
test -f merged/usr/bin/perl5.36.0
grep binary_content merged/usr/bin/perl5.36.0

umount merged
rm -rf lower upper workdir merged

echo "All hard link tests passed!"
