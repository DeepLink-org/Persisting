#!/bin/bash
# Test special file operations: mknod (devices, FIFOs, sockets),
# fallocate, statfs, setattr (chmod/chown/truncate/utimens).

set -xeuo pipefail

cleanup() {
    cd /
    umount "$MERGED" 2>/dev/null || true
    rm -rf "$TESTDIR"
}

TESTDIR=$(mktemp -d /tmp/test-special.XXXXXX)
trap cleanup EXIT
cd "$TESTDIR"

MERGED="$TESTDIR/merged"

# Helper: check if mknod for device nodes works (fails in user namespaces)
CAN_MKNOD=true
if ! mknod "$TESTDIR/.mknod_test" c 1 3 2>/dev/null; then
    CAN_MKNOD=false
fi
rm -f "$TESTDIR/.mknod_test"

# ========================================
# Test 1: Create character device
# ========================================
if $CAN_MKNOD; then
echo "=== Test 1: mknod character device ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir,dev merged

mknod merged/chardev c 1 3  # /dev/null equivalent
test -c merged/chardev
stat -c '%F' merged/chardev | grep "character special"

# Verify major:minor
test "$(stat -c '%t:%T' merged/chardev)" = "1:3"

umount merged
rm -rf lower upper workdir merged
else
echo "=== Test 1: SKIPPED (mknod not available) ==="
fi

# ========================================
# Test 2: Create block device
# ========================================
if $CAN_MKNOD; then
echo "=== Test 2: mknod block device ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir,dev merged

mknod merged/blockdev b 7 0
test -b merged/blockdev
stat -c '%F' merged/blockdev | grep "block special"
test "$(stat -c '%t:%T' merged/blockdev)" = "7:0"

umount merged
rm -rf lower upper workdir merged
else
echo "=== Test 2: SKIPPED (mknod not available) ==="
fi

# ========================================
# Test 3: Create FIFO (named pipe)
# ========================================
echo "=== Test 3: mkfifo ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

mkfifo merged/myfifo
test -p merged/myfifo
stat -c '%F' merged/myfifo | grep "fifo"

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 4: Create socket
# ========================================
echo "=== Test 4: Unix socket ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

python3 -c "import socket; s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.bind('merged/mysock')"
test -S merged/mysock
stat -c '%F' merged/mysock | grep "socket"

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 5: Fallocate
# ========================================
echo "=== Test 5: Fallocate ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "test" > merged/fallocfile
fallocate -l 1048576 merged/fallocfile
test $(stat -c %s merged/fallocfile) -ge 1048576

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 6: Statfs
# ========================================
echo "=== Test 6: Statfs ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

# statfs should return valid data
stat -f merged | grep -i "Type:"
BLOCKS=$(stat -f -c '%b' merged)
test "$BLOCKS" -gt 0

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 7: chmod
# ========================================
echo "=== Test 7: chmod ==="
mkdir -p lower upper workdir merged

echo "test" > lower/chmodfile

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

chmod 0700 merged/chmodfile
test "$(stat -c %a merged/chmodfile)" = "700"

chmod 0644 merged/chmodfile
test "$(stat -c %a merged/chmodfile)" = "644"

chmod 0000 merged/chmodfile
test "$(stat -c %a merged/chmodfile)" = "0"

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 8: chown
# ========================================
echo "=== Test 8: chown ==="
mkdir -p lower upper workdir merged

echo "test" > lower/chownfile

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

chown 1000:1000 merged/chownfile
test "$(stat -c '%u:%g' merged/chownfile)" = "1000:1000"

chown 0:0 merged/chownfile
test "$(stat -c '%u:%g' merged/chownfile)" = "0:0"

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 9: Truncate
# ========================================
echo "=== Test 9: Truncate ==="
mkdir -p lower upper workdir merged

dd if=/dev/zero of=lower/truncfile bs=1024 count=100 2>/dev/null

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

test $(stat -c %s merged/truncfile) -eq 102400

truncate -s 50000 merged/truncfile
test $(stat -c %s merged/truncfile) -eq 50000

truncate -s 200000 merged/truncfile
test $(stat -c %s merged/truncfile) -eq 200000

truncate -s 0 merged/truncfile
test $(stat -c %s merged/truncfile) -eq 0

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 10: utimens (set timestamps)
# ========================================
echo "=== Test 10: utimens ==="
mkdir -p lower upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

echo "test" > merged/tsfile
touch -d "2021-03-15 14:30:00" merged/tsfile
stat --format "%y" merged/tsfile | grep "14:30:00"

# Test on directory
mkdir merged/tsdir
touch -d "2022-07-20 09:15:30" merged/tsdir
stat --format "%y" merged/tsdir | grep "09:15:30"

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 11: Device in lower layer preserved
# ========================================
if $CAN_MKNOD; then
echo "=== Test 11: Device in lower layer ==="
mkdir -p lower upper workdir merged

mknod lower/lowerdev c 5 1
chmod 0666 lower/lowerdev

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir,dev merged

test -c merged/lowerdev
test "$(stat -c '%t:%T' merged/lowerdev)" = "5:1"

umount merged
rm -rf lower upper workdir merged
else
echo "=== Test 11: SKIPPED (mknod not available) ==="
fi

# ========================================
# Test 12: FIFO in lower layer preserved
# ========================================
echo "=== Test 12: FIFO in lower layer ==="
mkdir -p lower upper workdir merged

mkfifo lower/lowerfifo

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

test -p merged/lowerfifo

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 13: chmod on directory
# ========================================
echo "=== Test 13: chmod on directory ==="
mkdir -p lower/subdir upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

chmod 0700 merged/subdir
test "$(stat -c %a merged/subdir)" = "700"

chmod 0755 merged/subdir
test "$(stat -c %a merged/subdir)" = "755"

umount merged
rm -rf lower upper workdir merged

# ========================================
# Test 14: chown on directory
# ========================================
echo "=== Test 14: chown on directory ==="
mkdir -p lower/chowndir upper workdir merged

fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

chown 500:500 merged/chowndir
test "$(stat -c '%u:%g' merged/chowndir)" = "500:500"

umount merged
rm -rf lower upper workdir merged

echo "All special file tests passed!"
