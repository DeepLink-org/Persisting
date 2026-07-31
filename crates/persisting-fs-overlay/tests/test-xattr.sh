#!/bin/bash
# Test extended attribute operations: set, get, list, remove,
# internal xattr filtering, large xattrs, xattr on directories.

set -xeuo pipefail

cleanup() {
    cd /
    umount "$MERGED" 2>/dev/null || true
    rm -rf "$TESTDIR"
}

TESTDIR=$(mktemp -d /tmp/test-xattr.XXXXXX)
trap cleanup EXIT
cd "$TESTDIR"

MERGED="$TESTDIR/merged"

# ========================================
# Test 1: Basic set/get xattr
# ========================================
echo "=== Test 1: Basic set/get xattr ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "test" > merged/xfile
setfattr -n user.myattr -v "myvalue" merged/xfile
getfattr --only-values -n user.myattr merged/xfile | grep myvalue

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 2: List xattrs
# ========================================
echo "=== Test 2: List xattrs ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "test" > merged/xlist
setfattr -n user.attr1 -v "val1" merged/xlist
setfattr -n user.attr2 -v "val2" merged/xlist
setfattr -n user.attr3 -v "val3" merged/xlist

# List should contain all three
getfattr -d merged/xlist | grep user.attr1
getfattr -d merged/xlist | grep user.attr2
getfattr -d merged/xlist | grep user.attr3

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 3: Remove xattr
# ========================================
echo "=== Test 3: Remove xattr ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "test" > merged/xremove
setfattr -n user.toremove -v "removeme" merged/xremove
getfattr --only-values -n user.toremove merged/xremove | grep removeme

setfattr -x user.toremove merged/xremove
if getfattr --only-values -n user.toremove merged/xremove 2>/dev/null; then
    echo "ERROR: xattr should have been removed"
    exit 1
fi

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 4: Xattr on directories
# ========================================
echo "=== Test 4: Xattr on directories ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

mkdir merged/xdir
setfattr -n user.dirattr -v "dirval" merged/xdir
getfattr --only-values -n user.dirattr merged/xdir | grep dirval

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 5: Xattr from lower layer visible
# ========================================
echo "=== Test 5: Lower layer xattr visible ==="
mkdir -p lower upper workdir merged

echo "lower_file" > lower/lxfile
setfattr -n user.lower_attr -v "from_lower" lower/lxfile

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

getfattr --only-values -n user.lower_attr merged/lxfile | grep from_lower

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 6: Internal xattrs filtered from listing
# ========================================
echo "=== Test 6: Internal xattrs filtered ==="
mkdir -p lower upper workdir merged

echo "test" > lower/filtered
# Set internal xattr directly on lower
setfattr -n user.fuseoverlayfs.origin -v "internal" lower/filtered 2>/dev/null || true

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

# Internal xattrs should NOT appear in listing
if getfattr -d merged/filtered 2>/dev/null | grep "fuseoverlayfs"; then
    echo "ERROR: Internal xattr should be filtered"
    exit 1
fi

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 7: Large xattr values
# ========================================
echo "=== Test 7: Large xattr values ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "test" > merged/bigxattr
# Create a large xattr value (up to filesystem limit, typically ~4K or ~64K)
BIGVAL=$(python3 -c "print('A' * 4000)")
setfattr -n user.bigattr -v "$BIGVAL" merged/bigxattr
GOTVAL=$(getfattr --only-values -n user.bigattr merged/bigxattr)
test ${#GOTVAL} -eq 4000

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 8: Xattr preserved across copy-up
# ========================================
echo "=== Test 8: Xattr preserved across copy-up ==="
mkdir -p lower upper workdir merged

echo "data" > lower/xcopyup
setfattr -n user.preserved -v "keep_me" lower/xcopyup

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

# Trigger copy-up
echo "more" >> merged/xcopyup

# Xattr should still be there
getfattr --only-values -n user.preserved merged/xcopyup | grep keep_me

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 9: Set xattr on lower-layer file triggers copy-up
# ========================================
echo "=== Test 9: Set xattr triggers copy-up ==="
mkdir -p lower upper workdir merged

echo "lower" > lower/xsetcopyup

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

setfattr -n user.newattr -v "newval" merged/xsetcopyup
getfattr --only-values -n user.newattr merged/xsetcopyup | grep newval

# File should now be in upper
test -f upper/xsetcopyup

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 10: Multiple xattrs on same file
# ========================================
echo "=== Test 10: Multiple xattrs ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "multi" > merged/multiattr
for i in $(seq 1 10); do
    setfattr -n "user.attr_${i}" -v "value_${i}" merged/multiattr
done

for i in $(seq 1 10); do
    getfattr --only-values -n "user.attr_${i}" merged/multiattr | grep "value_${i}"
done

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 11: noxattrs option
# ========================================
echo "=== Test 11: noxattrs option ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o noxattrs=1,lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "test" > merged/noxfile
if setfattr -n user.foo -v bar merged/noxfile 2>/dev/null; then
    echo "ERROR: xattr should be disabled"
    exit 1
fi

umount merged
rm -rf lower upper workdir merged

echo "All xattr tests passed!"
