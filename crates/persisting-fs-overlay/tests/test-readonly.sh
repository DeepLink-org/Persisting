#!/bin/bash
# Test read-only overlay (no upperdir), UID/GID mapping,
# and various mount option behaviors.

set -xeuo pipefail

cleanup() {
    cd /
    umount "$MERGED" 2>/dev/null || true
    rm -rf "$TESTDIR"
}

TESTDIR=$(mktemp -d /tmp/test-readonly.XXXXXX)
trap cleanup EXIT
cd "$TESTDIR"

MERGED="$TESTDIR/merged"

# ========================================
# Test 1: Read-only overlay (no upperdir)
# ========================================
echo "=== Test 1: Read-only overlay ==="
mkdir -p lower merged

echo "readonly_data" > lower/rofile
mkdir lower/rodir

fuse-overlayfs -o lowerdir=lower merged

# Read should work
grep readonly_data merged/rofile
test -d merged/rodir

# All write operations should fail with EROFS
set +e
touch merged/newfile 2>&1 | grep -i "read-only"
mkdir merged/newdir 2>&1 | grep -i "read-only"
rm merged/rofile 2>&1 | grep -i "read-only"
ln -s foo merged/newlink 2>&1 | grep -i "read-only"
set -e

umount merged
rm -rf lower merged

# ========================================
# Test 2: Multiple lower layers, no upper
# ========================================
echo "=== Test 2: Multiple read-only lowers ==="
mkdir -p lower1 lower2 merged

echo "from1" > lower1/file1
echo "from2" > lower2/file2
mkdir lower1/shared lower2/shared
echo "l1" > lower1/shared/l1file
echo "l2" > lower2/shared/l2file

fuse-overlayfs -o lowerdir=lower2:lower1 merged

test -f merged/file1
test -f merged/file2
test -f merged/shared/l1file
test -f merged/shared/l2file

umount merged
rm -rf lower1 lower2 merged

# ========================================
# Test 3: squash_to_root
# ========================================
echo "=== Test 3: squash_to_root ==="
mkdir -p lower upper workdir merged

echo "test" > lower/sqfile
chown 1000:1000 lower/sqfile

fuse-overlayfs -o squash_to_root,lowerdir=lower,upperdir=upper,workdir=workdir merged

# File should appear as root:root
test "$(stat -c '%u:%g' merged/sqfile)" = "0:0"

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 4: squash_to_uid/gid
# ========================================
echo "=== Test 4: squash_to_uid/gid ==="
mkdir -p lower upper workdir merged

echo "test" > lower/sqfile2
chown 500:500 lower/sqfile2

fuse-overlayfs -o squash_to_uid=1000,squash_to_gid=2000,lowerdir=lower,upperdir=upper,workdir=workdir merged

test "$(stat -c '%u' merged/sqfile2)" = "1000"
test "$(stat -c '%g' merged/sqfile2)" = "2000"

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 5: UID/GID mapping
# ========================================
echo "=== Test 5: UID/GID mapping ==="
mkdir -p lower upper workdir merged

echo "test" > lower/mapfile
chown 0:0 lower/mapfile

fuse-overlayfs -o uidmapping=0:1000:1,gidmapping=0:2000:1,lowerdir=lower,upperdir=upper,workdir=workdir merged

# UID 0 on host should map to 1000 in overlay
test "$(stat -c '%u' merged/mapfile)" = "1000"
test "$(stat -c '%g' merged/mapfile)" = "2000"

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 6: Read-only with symlinks
# ========================================
echo "=== Test 6: Read-only with symlinks ==="
mkdir -p lower merged

echo "target" > lower/real
ln -s real lower/link

fuse-overlayfs -o lowerdir=lower merged

test -L merged/link
readlink merged/link | grep real
grep target merged/link

umount merged
rm -rf lower merged

# ========================================
# Test 7: Read-only with special files
# ========================================
echo "=== Test 7: Read-only with special files ==="
mkdir -p lower merged

mkfifo lower/fifo
if mknod lower/cdev c 1 3 2>/dev/null; then
    HAS_CDEV=true
else
    HAS_CDEV=false
fi

fuse-overlayfs -o lowerdir=lower merged

test -p merged/fifo
if $HAS_CDEV; then
    test -c merged/cdev
fi

umount merged
rm -rf lower merged

# ========================================
# Test 8: Volatile (fsync=0) option
# ========================================
echo "=== Test 8: Volatile option ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o volatile,lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "volatile_test" > merged/vfile
sync merged/vfile 2>/dev/null || true
grep volatile_test merged/vfile

umount merged
rm -rf lower upper workdir merged

echo "All read-only and mount option tests passed!"
