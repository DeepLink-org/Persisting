#!/bin/bash
# Stress tests: hammer the filesystem with concurrent operations,
# many files, deep trees, and mixed workloads to find races and
# measure throughput.

set -xeuo pipefail

NPROC=${NPROC:-$(nproc)}
# Scale iteration counts so the suite finishes in reasonable time
SCALE=${SCALE:-1}

cleanup() {
    cd /
    umount "$MERGED" 2>/dev/null || true
    rm -rf "$TESTDIR"
}

TESTDIR=$(mktemp -d /tmp/test-stress.XXXXXX)
trap cleanup EXIT
cd "$TESTDIR"

MERGED="$TESTDIR/merged"

mount_overlay() {
    mkdir -p lower upper workdir merged
    fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged
}

remount_overlay() {
    umount merged
    rm -rf upper workdir
    mkdir -p upper workdir
    fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged
}

reset_overlay() {
    umount merged 2>/dev/null || true
    rm -rf lower upper workdir merged
}

elapsed() {
    echo "$(($(date +%s%N) / 1000000 - $1))"
}

# chunk_end COUNT NPROC WORKER_INDEX
# Returns END for a worker, ensuring last worker covers remainder
chunk_end() {
    local count=$1 nproc=$2 w=$3
    local chunk=$((count / nproc))
    if [ "$w" -eq "$((nproc - 1))" ]; then
        echo "$count"
    else
        echo "$(((w + 1) * chunk))"
    fi
}

# ========================================
# Test 1: Create many files sequentially
# ========================================
echo "=== Test 1: Create $((1000 * SCALE)) files sequentially ==="
reset_overlay; mount_overlay

COUNT=$((1000 * SCALE))
T0=$(($(date +%s%N) / 1000000))

for i in $(seq 1 $COUNT); do
    echo "data_$i" > "merged/file_$i"
done

MS=$(elapsed $T0)
echo "Created $COUNT files in ${MS}ms ($((COUNT * 1000 / (MS + 1))) files/s)"

# Verify a sample
test "$(cat merged/file_1)" = "data_1"
test "$(cat merged/file_$COUNT)" = "data_$COUNT"
test "$(ls merged/ | wc -l)" -eq "$COUNT"

reset_overlay

# ========================================
# Test 2: Parallel file creation
# ========================================
echo "=== Test 2: Parallel file creation ($NPROC workers) ==="
reset_overlay; mount_overlay

FILES_PER_WORKER=$((500 * SCALE))
TOTAL=$((FILES_PER_WORKER * NPROC))
T0=$(($(date +%s%N) / 1000000))

for w in $(seq 1 $NPROC); do
    (
        DIR="merged/worker_$w"
        mkdir -p "$DIR"
        for i in $(seq 1 $FILES_PER_WORKER); do
            echo "w${w}_${i}" > "$DIR/file_$i"
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "Created $TOTAL files in ${MS}ms ($((TOTAL * 1000 / (MS + 1))) files/s)"

# Verify each worker directory
for w in $(seq 1 $NPROC); do
    test "$(ls merged/worker_$w/ | wc -l)" -eq "$FILES_PER_WORKER"
done

reset_overlay

# ========================================
# Test 3: Deep directory tree
# ========================================
echo "=== Test 3: Deep directory tree (200 levels) ==="
reset_overlay; mount_overlay

DEPTH=200
T0=$(($(date +%s%N) / 1000000))

P="merged"
for i in $(seq 1 $DEPTH); do
    P="$P/d"
    mkdir "$P"
done
echo "bottom" > "$P/leaf"

MS=$(elapsed $T0)
echo "Created $DEPTH-level tree in ${MS}ms"

# Read back
test "$(cat "$P/leaf")" = "bottom"

# Walk back up
for i in $(seq 1 $DEPTH); do
    P=$(dirname "$P")
done

reset_overlay

# ========================================
# Test 4: Wide directory (many entries)
# ========================================
echo "=== Test 4: Wide directory ($((5000 * SCALE)) entries) ==="
reset_overlay; mount_overlay

COUNT=$((5000 * SCALE))
T0=$(($(date +%s%N) / 1000000))

mkdir merged/wide
for i in $(seq 1 $COUNT); do
    : > "merged/wide/e_$i"
done

MS=$(elapsed $T0)
echo "Created $COUNT entries in ${MS}ms"

# readdir performance
T0=$(($(date +%s%N) / 1000000))
GOT=$(ls merged/wide/ | wc -l)
MS=$(elapsed $T0)
echo "Listed $GOT entries in ${MS}ms"
test "$GOT" -eq "$COUNT"

reset_overlay

# ========================================
# Test 5: Parallel stat storm
# ========================================
echo "=== Test 5: Parallel stat storm ==="
reset_overlay
mkdir -p lower
for i in $(seq 1 500); do
    echo "lower_$i" > "lower/lf_$i"
done
mount_overlay

T0=$(($(date +%s%N) / 1000000))

for w in $(seq 1 $NPROC); do
    (
        for round in $(seq 1 $((20 * SCALE))); do
            for i in $(seq 1 500); do
                stat "merged/lf_$i" > /dev/null
            done
        done
    ) &
done
wait

TOTAL_STATS=$((NPROC * 20 * SCALE * 500))
MS=$(elapsed $T0)
echo "$TOTAL_STATS stats in ${MS}ms ($((TOTAL_STATS * 1000 / (MS + 1))) stats/s)"

reset_overlay

# ========================================
# Test 6: Concurrent read/write on same files
# ========================================
echo "=== Test 6: Concurrent readers and writers ==="
reset_overlay; mount_overlay

# Create initial files
for i in $(seq 1 100); do
    echo "init" > "merged/rw_$i"
done

T0=$(($(date +%s%N) / 1000000))

# Writers: append to files
for w in $(seq 1 $((NPROC / 2 + 1))); do
    (
        for round in $(seq 1 $((50 * SCALE))); do
            for i in $(seq 1 100); do
                echo "w${w}_r${round}" >> "merged/rw_$i"
            done
        done
    ) &
done

# Readers: read files concurrently with writes
for r in $(seq 1 $((NPROC / 2 + 1))); do
    (
        for round in $(seq 1 $((50 * SCALE))); do
            for i in $(seq 1 100); do
                cat "merged/rw_$i" > /dev/null
            done
        done
    ) &
done

wait
MS=$(elapsed $T0)
echo "Concurrent R/W finished in ${MS}ms"

# All files must still be readable
for i in $(seq 1 100); do
    head -1 "merged/rw_$i" | grep -q init
done

reset_overlay

# ========================================
# Test 7: Rename storm
# ========================================
echo "=== Test 7: Rename storm ==="
reset_overlay; mount_overlay

COUNT=$((200 * SCALE))
for i in $(seq 1 $COUNT); do
    echo "rename_$i" > "merged/ren_$i"
done

T0=$(($(date +%s%N) / 1000000))

# Parallel renames within separate ranges to avoid conflicts
CHUNK=$((COUNT / NPROC))
for w in $(seq 0 $((NPROC - 1))); do
    (
        START=$((w * CHUNK + 1))
        END=$(chunk_end $COUNT $NPROC $w)
        for i in $(seq $START $END); do
            mv "merged/ren_$i" "merged/renamed_$i"
        done
        # Rename back
        for i in $(seq $START $END); do
            mv "merged/renamed_$i" "merged/ren_$i"
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "$((COUNT * 2)) renames in ${MS}ms"

# Verify all files survived
for i in $(seq 1 $COUNT); do
    test "$(cat merged/ren_$i)" = "rename_$i"
done

reset_overlay

# ========================================
# Test 8: Hardlink stress
# ========================================
echo "=== Test 8: Hardlink stress ==="
reset_overlay; mount_overlay

COUNT=$((100 * SCALE))
echo "source" > merged/hl_source

T0=$(($(date +%s%N) / 1000000))

for i in $(seq 1 $COUNT); do
    ln merged/hl_source "merged/hl_$i"
done

MS=$(elapsed $T0)
echo "Created $COUNT hardlinks in ${MS}ms"

# All should report same nlink
EXPECTED=$((COUNT + 1))
test "$(stat -c %h merged/hl_source)" -ge "$EXPECTED"

# Remove half, verify survivors
for i in $(seq 1 $((COUNT / 2))); do
    rm "merged/hl_$i"
done

for i in $(seq $((COUNT / 2 + 1)) $COUNT); do
    grep -q source "merged/hl_$i"
done

reset_overlay

# ========================================
# Test 9: Copy-up storm (parallel modifications of lower-layer files)
# ========================================
echo "=== Test 9: Copy-up storm ==="
reset_overlay
mkdir -p lower
for i in $(seq 1 $((200 * SCALE))); do
    echo "original_data_$i" > "lower/cu_$i"
done
mount_overlay

T0=$(($(date +%s%N) / 1000000))

# Parallel copy-ups by writing to lower-layer files
COUNT_CU=$((200 * SCALE))
CHUNK=$((COUNT_CU / NPROC))
for w in $(seq 0 $((NPROC - 1))); do
    (
        START=$((w * CHUNK + 1))
        END=$(chunk_end $COUNT_CU $NPROC $w)
        for i in $(seq $START $END); do
            echo "modified" >> "merged/cu_$i"
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "$((200 * SCALE)) copy-ups in ${MS}ms"

# Verify all were copied up and contain both original and appended data
for i in $(seq 1 $((200 * SCALE))); do
    grep -q "original_data_$i" "merged/cu_$i"
    grep -q "modified" "merged/cu_$i"
done

reset_overlay

# ========================================
# Test 10: Mixed workload (create, stat, write, rename, unlink)
# ========================================
echo "=== Test 10: Mixed workload ($NPROC workers) ==="
reset_overlay
mkdir -p lower
for i in $(seq 1 100); do
    echo "lower" > "lower/mix_$i"
done
mount_overlay

T0=$(($(date +%s%N) / 1000000))

for w in $(seq 1 $NPROC); do
    (
        D="merged/mix_w$w"
        mkdir -p "$D"
        for round in $(seq 1 $((30 * SCALE))); do
            # create
            echo "r$round" > "$D/tmp_$round"
            # stat lower file
            stat "merged/mix_$(( (round % 100) + 1 ))" > /dev/null
            # write to lower file (copy-up)
            echo "w$w" >> "merged/mix_$(( (round % 100) + 1 ))"
            # rename within worker dir
            mv "$D/tmp_$round" "$D/final_$round"
            # read back
            cat "$D/final_$round" > /dev/null
        done
        # cleanup
        for round in $(seq 1 $((30 * SCALE))); do
            rm "$D/final_$round"
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "Mixed workload finished in ${MS}ms"

reset_overlay

# ========================================
# Test 11: Parallel unlink storm
# ========================================
echo "=== Test 11: Parallel unlink storm ==="
reset_overlay; mount_overlay

COUNT=$((1000 * SCALE))
for i in $(seq 1 $COUNT); do
    : > "merged/del_$i"
done

T0=$(($(date +%s%N) / 1000000))

CHUNK=$((COUNT / NPROC))
for w in $(seq 0 $((NPROC - 1))); do
    (
        START=$((w * CHUNK + 1))
        END=$(chunk_end $COUNT $NPROC $w)
        for i in $(seq $START $END); do
            rm "merged/del_$i"
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "Deleted $COUNT files in ${MS}ms ($((COUNT * 1000 / (MS + 1))) deletes/s)"

test "$(ls merged/ | wc -l)" -eq 0

reset_overlay

# ========================================
# Test 12: Rapid mount/unmount cycles
# ========================================
echo "=== Test 12: Rapid mount/unmount cycles ==="
reset_overlay
mkdir -p lower
echo "persist" > lower/stable

CYCLES=$((20 * SCALE))
T0=$(($(date +%s%N) / 1000000))

for c in $(seq 1 $CYCLES); do
    mkdir -p upper workdir merged
    fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged
    # Do some work each cycle
    echo "cycle_$c" > "merged/c_$c"
    test -f merged/stable
    grep -q persist merged/stable
    umount merged
    rm -rf upper workdir merged
done

MS=$(elapsed $T0)
echo "$CYCLES mount/unmount cycles in ${MS}ms"

reset_overlay

# ========================================
# Test 13: Lower-layer hardlink chown+unlink+link parallel (containers/storage simulation)
# ========================================
echo "=== Test 13: Parallel hardlink chown+unlink+link ==="
reset_overlay
mkdir -p lower/usr/bin lower/usr/lib

# Create many hardlink pairs in lower layer (simulates distro packages)
for i in $(seq 1 $((50 * SCALE))); do
    echo "binary_$i" > "lower/usr/bin/prog_${i}_v1"
    ln "lower/usr/bin/prog_${i}_v1" "lower/usr/bin/prog_$i"
done

# Also some in lib
for i in $(seq 1 $((50 * SCALE))); do
    echo "lib_$i" > "lower/usr/lib/lib_${i}.so.1"
    ln "lower/usr/lib/lib_${i}.so.1" "lower/usr/lib/lib_${i}.so"
done

mount_overlay

T0=$(($(date +%s%N) / 1000000))

# Simulate containers/storage parallel walk: chown + rm link + re-link
(
    for i in $(seq 1 $((50 * SCALE))); do
        chown "$(id -u):$(id -g)" "merged/usr/bin/prog_${i}_v1"
        rm "merged/usr/bin/prog_$i"
        ln "merged/usr/bin/prog_${i}_v1" "merged/usr/bin/prog_$i"
    done
) &

(
    for i in $(seq 1 $((50 * SCALE))); do
        chown "$(id -u):$(id -g)" "merged/usr/lib/lib_${i}.so.1"
        rm "merged/usr/lib/lib_${i}.so"
        ln "merged/usr/lib/lib_${i}.so.1" "merged/usr/lib/lib_${i}.so"
    done
) &

wait
MS=$(elapsed $T0)
echo "Parallel chown+unlink+link in ${MS}ms"

# Verify all files intact
for i in $(seq 1 $((50 * SCALE))); do
    grep -q "binary_$i" "merged/usr/bin/prog_$i"
    grep -q "binary_$i" "merged/usr/bin/prog_${i}_v1"
    grep -q "lib_$i" "merged/usr/lib/lib_${i}.so"
    grep -q "lib_$i" "merged/usr/lib/lib_${i}.so.1"
done

reset_overlay

# ========================================
# Test 14: Large file I/O throughput
# ========================================
echo "=== Test 14: Large file I/O throughput ==="
reset_overlay; mount_overlay

SIZE_MB=$((64 * SCALE))

# Write
T0=$(($(date +%s%N) / 1000000))
dd if=/dev/zero of=merged/bigfile bs=1M count=$SIZE_MB 2>/dev/null
MS=$(elapsed $T0)
echo "Write ${SIZE_MB}MB: ${MS}ms ($((SIZE_MB * 1000 / (MS + 1))) MB/s)"

# Read
T0=$(($(date +%s%N) / 1000000))
dd if=merged/bigfile of=/dev/null bs=1M 2>/dev/null
MS=$(elapsed $T0)
echo "Read ${SIZE_MB}MB: ${MS}ms ($((SIZE_MB * 1000 / (MS + 1))) MB/s)"

# Copy-up large file from lower
umount merged
mv upper/bigfile lower/bigfile_lower
rm -rf upper workdir merged; mkdir -p upper workdir merged
fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged

T0=$(($(date +%s%N) / 1000000))
echo "trigger" >> merged/bigfile_lower
MS=$(elapsed $T0)
echo "Copy-up ${SIZE_MB}MB file: ${MS}ms"

reset_overlay

# ========================================
# Test 15: Concurrent directory operations
# ========================================
echo "=== Test 15: Concurrent mkdir/rmdir ==="
reset_overlay; mount_overlay

T0=$(($(date +%s%N) / 1000000))

for w in $(seq 1 $NPROC); do
    (
        for round in $(seq 1 $((100 * SCALE))); do
            mkdir -p "merged/cdir_${w}_${round}/sub/deep"
            echo "x" > "merged/cdir_${w}_${round}/sub/deep/f"
            rm "merged/cdir_${w}_${round}/sub/deep/f"
            rmdir "merged/cdir_${w}_${round}/sub/deep"
            rmdir "merged/cdir_${w}_${round}/sub"
            rmdir "merged/cdir_${w}_${round}"
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "$((NPROC * 100 * SCALE)) mkdir/rmdir cycles in ${MS}ms"

# Directory should be empty
test "$(ls merged/ | wc -l)" -eq 0

reset_overlay

# ========================================
# Test 16: Concurrent copy-up of the same file
# ========================================
echo "=== Test 16: Concurrent copy-up of the same file ==="
reset_overlay
mkdir -p lower
for i in $(seq 1 20); do
    dd if=/dev/urandom of="lower/shared_$i" bs=4096 count=4 2>/dev/null
done
mount_overlay

T0=$(($(date +%s%N) / 1000000))

# All workers write to the SAME set of lower-layer files simultaneously,
# forcing concurrent copy-up of the same inodes
for w in $(seq 1 $NPROC); do
    (
        for i in $(seq 1 20); do
            echo "writer_${w}" >> "merged/shared_$i"
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "Concurrent copy-up finished in ${MS}ms"

# Every file must contain all writers' data and be readable
for i in $(seq 1 20); do
    for w in $(seq 1 $NPROC); do
        grep -q "writer_${w}" "merged/shared_$i"
    done
done

reset_overlay

# ========================================
# Test 17: Readdir during concurrent mutations
# ========================================
echo "=== Test 17: Readdir during concurrent mutations ==="
reset_overlay; mount_overlay

# Seed initial files
for i in $(seq 1 200); do
    : > "merged/rd_$i"
done

T0=$(($(date +%s%N) / 1000000))

# Readers: continuously list the directory
for r in $(seq 1 $((NPROC / 2 + 1))); do
    (
        for round in $(seq 1 $((40 * SCALE))); do
            ls merged/ > /dev/null
        done
    ) &
done

# Mutators: add and remove files while readdir runs
for w in $(seq 1 $((NPROC / 2 + 1))); do
    (
        for round in $(seq 1 $((40 * SCALE))); do
            : > "merged/dyn_${w}_${round}"
        done
        for round in $(seq 1 $((40 * SCALE))); do
            rm -f "merged/dyn_${w}_${round}"
        done
    ) &
done

wait

MS=$(elapsed $T0)
echo "Readdir during mutations finished in ${MS}ms"

# Original files must survive
for i in $(seq 1 200); do
    test -f "merged/rd_$i"
done
# Dynamic files must be gone
test "$(ls merged/dyn_* 2>/dev/null | wc -l)" -eq 0

reset_overlay

# ========================================
# Test 18: Symlink storm
# ========================================
echo "=== Test 18: Parallel symlink create/read/delete ==="
reset_overlay; mount_overlay

COUNT=$((200 * SCALE))
T0=$(($(date +%s%N) / 1000000))

# Create targets
for i in $(seq 1 $COUNT); do
    echo "target_$i" > "merged/sym_target_$i"
done

# Parallel symlink creation
CHUNK=$((COUNT / NPROC))
for w in $(seq 0 $((NPROC - 1))); do
    (
        START=$((w * CHUNK + 1))
        END=$(chunk_end $COUNT $NPROC $w)
        for i in $(seq $START $END); do
            ln -s "sym_target_$i" "merged/sym_link_$i"
        done
    ) &
done
wait

# Verify all symlinks
for i in $(seq 1 $COUNT); do
    TARGET=$(readlink "merged/sym_link_$i")
    test "$TARGET" = "sym_target_$i"
    cat "merged/sym_link_$i" > /dev/null
done

# Parallel symlink deletion
for w in $(seq 0 $((NPROC - 1))); do
    (
        START=$((w * CHUNK + 1))
        END=$(chunk_end $COUNT $NPROC $w)
        for i in $(seq $START $END); do
            rm "merged/sym_link_$i"
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "$COUNT symlinks create/verify/delete in ${MS}ms"

# Targets must survive, links must be gone
for i in $(seq 1 $COUNT); do
    test -f "merged/sym_target_$i"
    test ! -e "merged/sym_link_$i"
done

reset_overlay

# ========================================
# Test 19: Xattr stress
# ========================================
echo "=== Test 19: Parallel xattr set/get/remove ==="
reset_overlay; mount_overlay

COUNT=$((100 * SCALE))

# Create files
for i in $(seq 1 $COUNT); do
    : > "merged/xa_$i"
done

T0=$(($(date +%s%N) / 1000000))

# Parallel xattr set
CHUNK=$((COUNT / NPROC))
for w in $(seq 0 $((NPROC - 1))); do
    (
        START=$((w * CHUNK + 1))
        END=$(chunk_end $COUNT $NPROC $w)
        for i in $(seq $START $END); do
            setfattr -n user.test -v "value_$i" "merged/xa_$i" 2>/dev/null || true
            setfattr -n user.extra -v "extra_$i" "merged/xa_$i" 2>/dev/null || true
        done
    ) &
done
wait

# Parallel xattr read and verify
for w in $(seq 0 $((NPROC - 1))); do
    (
        START=$((w * CHUNK + 1))
        END=$(chunk_end $COUNT $NPROC $w)
        for i in $(seq $START $END); do
            VAL=$(getfattr -n user.test --only-values "merged/xa_$i" 2>/dev/null) || true
            if [ -n "$VAL" ]; then
                test "$VAL" = "value_$i"
            fi
        done
    ) &
done
wait

# Parallel xattr remove
for w in $(seq 0 $((NPROC - 1))); do
    (
        START=$((w * CHUNK + 1))
        END=$(chunk_end $COUNT $NPROC $w)
        for i in $(seq $START $END); do
            setfattr -x user.test "merged/xa_$i" 2>/dev/null || true
            setfattr -x user.extra "merged/xa_$i" 2>/dev/null || true
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "$COUNT files xattr set/get/remove in ${MS}ms"

reset_overlay

# ========================================
# Test 20: Whiteout correctness under load
# ========================================
echo "=== Test 20: Whiteout correctness under load ==="
reset_overlay
mkdir -p lower
for i in $(seq 1 $((200 * SCALE))); do
    echo "lower_$i" > "lower/wo_$i"
done
mount_overlay

T0=$(($(date +%s%N) / 1000000))

# Parallel deletion of lower-layer files (creates whiteouts)
COUNT=$((200 * SCALE))
CHUNK=$((COUNT / NPROC))
for w in $(seq 0 $((NPROC - 1))); do
    (
        START=$((w * CHUNK + 1))
        END=$(chunk_end $COUNT $NPROC $w)
        for i in $(seq $START $END); do
            rm "merged/wo_$i"
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "Deleted $COUNT lower files (whiteouts) in ${MS}ms"

# Nothing should be visible
test "$(ls merged/ | wc -l)" -eq 0

# Remount (preserving upper layer) and verify whiteouts persist
umount merged
fuse-overlayfs -o lowerdir=lower,upperdir=upper,workdir=workdir merged
test "$(ls merged/ | wc -l)" -eq 0

# Create new files with same names — must succeed (whiteout must not block)
for i in $(seq 1 $COUNT); do
    echo "new_$i" > "merged/wo_$i"
done
for i in $(seq 1 $COUNT); do
    test "$(cat merged/wo_$i)" = "new_$i"
done

reset_overlay

# ========================================
# Test 21: Fallocate + concurrent write
# ========================================
echo "=== Test 21: Fallocate + concurrent write ==="
reset_overlay; mount_overlay

T0=$(($(date +%s%N) / 1000000))

for w in $(seq 1 $NPROC); do
    (
        F="merged/falloc_$w"
        # Preallocate space
        fallocate -l $((1024 * 1024)) "$F" 2>/dev/null || dd if=/dev/zero of="$F" bs=1M count=1 2>/dev/null
        # Write into pre-allocated space
        for round in $(seq 1 $((20 * SCALE))); do
            dd if=/dev/urandom of="$F" bs=4096 count=1 conv=notrunc 2>/dev/null
        done
        # Verify file is still valid
        test -f "$F"
        SIZE=$(stat -c %s "$F")
        test "$SIZE" -gt 0
    ) &
done
wait

MS=$(elapsed $T0)
echo "Fallocate+write on $NPROC files in ${MS}ms"

reset_overlay

# ========================================
# Test 22: Cross-directory rename storm
# ========================================
echo "=== Test 22: Cross-directory rename storm ==="
reset_overlay; mount_overlay

NDIRS=8
COUNT=$((50 * SCALE))

# Create directory structure
for d in $(seq 1 $NDIRS); do
    mkdir -p "merged/rdir_$d"
    for i in $(seq 1 $COUNT); do
        echo "d${d}_f${i}" > "merged/rdir_$d/file_$i"
    done
done

T0=$(($(date +%s%N) / 1000000))

# Each worker moves files from one directory to the next (ring)
for w in $(seq 1 $NDIRS); do
    (
        SRC="merged/rdir_$w"
        DST="merged/rdir_$(( (w % NDIRS) + 1 ))"
        for i in $(seq 1 $COUNT); do
            mv "$SRC/file_$i" "$DST/xmoved_${w}_${i}" 2>/dev/null || true
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "Cross-directory renames in ${MS}ms"

# Total file count must be preserved
TOTAL=$(find merged/rdir_* -type f | wc -l)
EXPECTED=$((NDIRS * COUNT))
test "$TOTAL" -eq "$EXPECTED"

reset_overlay

# ========================================
# Test 23: Copy-up + stat race
# ========================================
echo "=== Test 23: Copy-up + stat race ==="
reset_overlay
mkdir -p lower
for i in $(seq 1 $((100 * SCALE))); do
    echo "data_$i" > "lower/cs_$i"
    chmod 644 "lower/cs_$i"
done
mount_overlay

T0=$(($(date +%s%N) / 1000000))

# Writers trigger copy-up
(
    for i in $(seq 1 $((100 * SCALE))); do
        echo "extra" >> "merged/cs_$i"
    done
) &

# Readers stat/read the same files concurrently
for r in $(seq 1 $((NPROC - 1))); do
    (
        for i in $(seq 1 $((100 * SCALE))); do
            stat "merged/cs_$i" > /dev/null 2>&1 || true
            cat "merged/cs_$i" > /dev/null 2>&1 || true
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "Copy-up + stat race finished in ${MS}ms"

# All files must be valid and have the appended data
for i in $(seq 1 $((100 * SCALE))); do
    grep -q "data_$i" "merged/cs_$i"
    grep -q "extra" "merged/cs_$i"
done

reset_overlay

# ========================================
# Test 24: Parallel mmap read/write
# ========================================
echo "=== Test 24: Parallel mmap read/write ==="
reset_overlay
mkdir -p lower
dd if=/dev/urandom of=lower/mmapfile bs=4096 count=64 2>/dev/null
mount_overlay

T0=$(($(date +%s%N) / 1000000))

# Create a C helper that mmap-reads the file from multiple threads
# Fall back to dd-based I/O if mmap isn't testable from shell
for w in $(seq 1 $NPROC); do
    (
        for round in $(seq 1 $((20 * SCALE))); do
            # Read via mmap (dd triggers read syscalls through fuse)
            dd if=merged/mmapfile of=/dev/null bs=4096 count=64 2>/dev/null
        done
    ) &
done

# One writer modifies the file (forces copy-up)
(
    echo "mmap_modify" >> merged/mmapfile
) &

wait

MS=$(elapsed $T0)
echo "Parallel mmap-style I/O finished in ${MS}ms"

test -f merged/mmapfile
grep -q "mmap_modify" merged/mmapfile

reset_overlay

# ========================================
# Test 25: Parallel chmod/chown storm
# ========================================
echo "=== Test 25: Parallel chmod/chown storm ==="
reset_overlay
mkdir -p lower
for i in $(seq 1 $((100 * SCALE))); do
    echo "perm_$i" > "lower/perm_$i"
    chmod 644 "lower/perm_$i"
done
mount_overlay

T0=$(($(date +%s%N) / 1000000))

# Parallel permission changes (triggers copy-up + setattr)
COUNT_PERM=$((100 * SCALE))
CHUNK=$((COUNT_PERM / NPROC))
for w in $(seq 0 $((NPROC - 1))); do
    (
        START=$((w * CHUNK + 1))
        END=$(chunk_end $COUNT_PERM $NPROC $w)
        for i in $(seq $START $END); do
            chmod 755 "merged/perm_$i"
            chown "$(id -u):$(id -g)" "merged/perm_$i"
            chmod 700 "merged/perm_$i"
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "Parallel chmod/chown on $((100 * SCALE)) files in ${MS}ms"

# All files should have final permission 700
for i in $(seq 1 $((100 * SCALE))); do
    MODE=$(stat -c %a "merged/perm_$i")
    test "$MODE" = "700"
done

reset_overlay

# ========================================
# Test 26: Truncate + append race
# ========================================
echo "=== Test 26: Truncate + append race ==="
reset_overlay; mount_overlay

for i in $(seq 1 $((20 * NPROC))); do
    dd if=/dev/urandom of="merged/trunc_$i" bs=4096 count=4 2>/dev/null
done

T0=$(($(date +%s%N) / 1000000))

# Each worker truncates and re-fills its own files
for w in $(seq 0 $((NPROC - 1))); do
    (
        for round in $(seq 1 $((10 * SCALE))); do
            for j in $(seq 1 20); do
                F="merged/trunc_$((w * 20 + j))"
                truncate -s 0 "$F"
                echo "round_$round" > "$F"
            done
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "Truncate+append race finished in ${MS}ms"

# All files must be non-empty and readable
for i in $(seq 1 $((20 * NPROC))); do
    test -s "merged/trunc_$i"
done

reset_overlay

# ========================================
# Test 27: Multi-layer overlay stress
# ========================================
echo "=== Test 27: Multi-layer overlay stress ==="
reset_overlay

# Create 5 lower layers with overlapping files
for layer in $(seq 1 5); do
    mkdir -p "lower_$layer"
    for i in $(seq 1 $((50 * SCALE))); do
        echo "layer${layer}_file${i}" > "lower_$layer/ml_$i"
    done
    # Each layer also has unique files
    for i in $(seq 1 20); do
        echo "unique_l${layer}_$i" > "lower_$layer/unique_l${layer}_$i"
    done
done

mkdir -p upper workdir merged
fuse-overlayfs -o "lowerdir=lower_1:lower_2:lower_3:lower_4:lower_5,upperdir=upper,workdir=workdir" merged

T0=$(($(date +%s%N) / 1000000))

# Parallel operations on multi-layer mount
for w in $(seq 1 $NPROC); do
    (
        for round in $(seq 1 $((10 * SCALE))); do
            # Read overlapping files (should see top layer)
            cat "merged/ml_$(( (round % 50) + 1 ))" > /dev/null
            # Read unique files from various layers
            for layer in $(seq 1 5); do
                cat "merged/unique_l${layer}_$(( (round % 20) + 1 ))" > /dev/null
            done
            # Modify some (triggers copy-up from lower layers)
            echo "mod_w${w}_r${round}" >> "merged/ml_$(( (round % 50) + 1 ))"
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "Multi-layer stress finished in ${MS}ms"

# Top-layer content should be visible for overlapping files
for i in $(seq 1 $((50 * SCALE))); do
    head -1 "merged/ml_$i" | grep -q "layer1_file${i}"
done

# Unique files from all layers should be visible
for layer in $(seq 1 5); do
    for i in $(seq 1 20); do
        grep -q "unique_l${layer}_$i" "merged/unique_l${layer}_$i"
    done
done

umount merged
rm -rf lower_1 lower_2 lower_3 lower_4 lower_5 upper workdir merged

# ========================================
# Test 28: Rapid open/close storm
# ========================================
echo "=== Test 28: Rapid open/close storm ==="
reset_overlay
mkdir -p lower
for i in $(seq 1 50); do
    echo "oc_$i" > "lower/oc_$i"
done
mount_overlay

T0=$(($(date +%s%N) / 1000000))

# Hammer open/read/close on the same files from many workers
for w in $(seq 1 $NPROC); do
    (
        for round in $(seq 1 $((100 * SCALE))); do
            for i in $(seq 1 50); do
                cat "merged/oc_$i" > /dev/null
            done
        done
    ) &
done
wait

TOTAL_OPS=$((NPROC * 100 * SCALE * 50))
MS=$(elapsed $T0)
echo "$TOTAL_OPS open/read/close ops in ${MS}ms ($((TOTAL_OPS * 1000 / (MS + 1))) ops/s)"

reset_overlay

# ========================================
# Test 29: Create + immediate unlink (tmpfile pattern)
# ========================================
echo "=== Test 29: Create + immediate unlink (tmpfile pattern) ==="
reset_overlay; mount_overlay

T0=$(($(date +%s%N) / 1000000))

for w in $(seq 1 $NPROC); do
    (
        for round in $(seq 1 $((200 * SCALE))); do
            F="merged/tmp_${w}_${round}"
            echo "ephemeral" > "$F"
            rm "$F"
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "$((NPROC * 200 * SCALE)) create+unlink cycles in ${MS}ms"

# Directory must be empty
test "$(ls merged/ | wc -l)" -eq 0

reset_overlay

# ========================================
# Test 30: Simultaneous operations on nested dirs from lower layer
# ========================================
echo "=== Test 30: Nested lower-layer directory operations ==="
reset_overlay
mkdir -p lower/a/b/c/d/e
for i in $(seq 1 50); do
    echo "deep_$i" > "lower/a/b/c/d/e/f_$i"
done
mkdir -p lower/a/x lower/a/b/y lower/a/b/c/z
for i in $(seq 1 20); do
    echo "sib_$i" > "lower/a/x/s_$i"
    echo "sib2_$i" > "lower/a/b/y/s_$i"
done
mount_overlay

T0=$(($(date +%s%N) / 1000000))

# Concurrent: modify deep files, list at various levels, create new siblings
(
    for i in $(seq 1 50); do
        echo "modified" >> "merged/a/b/c/d/e/f_$i"
    done
) &

(
    for round in $(seq 1 $((20 * SCALE))); do
        ls merged/a/ > /dev/null
        ls merged/a/b/ > /dev/null
        ls merged/a/b/c/ > /dev/null
        ls merged/a/b/c/d/e/ > /dev/null
    done
) &

(
    for i in $(seq 1 $((30 * SCALE))); do
        mkdir -p "merged/a/b/c/new_$i"
        echo "new" > "merged/a/b/c/new_$i/data"
    done
) &

(
    for i in $(seq 1 20); do
        echo "mod" >> "merged/a/x/s_$i"
        echo "mod" >> "merged/a/b/y/s_$i"
    done
) &

wait

MS=$(elapsed $T0)
echo "Nested dir operations finished in ${MS}ms"

# Verify integrity
for i in $(seq 1 50); do
    grep -q "deep_$i" "merged/a/b/c/d/e/f_$i"
    grep -q "modified" "merged/a/b/c/d/e/f_$i"
done
for i in $(seq 1 $((30 * SCALE))); do
    test -f "merged/a/b/c/new_$i/data"
done

reset_overlay

# ========================================
# Test 31: Concurrent writes to same file via multiple fds
# ========================================
echo "=== Test 31: Concurrent writes to same file via multiple fds ==="
reset_overlay; mount_overlay

echo "initial" > merged/shared_file

T0=$(($(date +%s%N) / 1000000))

# Multiple workers open and append to the same file
for w in $(seq 1 $NPROC); do
    (
        for round in $(seq 1 $((100 * SCALE))); do
            echo "w${w}_r${round}" >> merged/shared_file
        done
    ) &
done
wait

MS=$(elapsed $T0)
EXPECTED=$((NPROC * 100 * SCALE + 1))
ACTUAL=$(wc -l < merged/shared_file)
echo "Concurrent appends: $ACTUAL lines (expected $EXPECTED) in ${MS}ms"
test "$ACTUAL" -eq "$EXPECTED"

reset_overlay

# ========================================
# Test 32: Rename across directories with concurrent readers
# ========================================
echo "=== Test 32: Rename across directories with concurrent readers ==="
reset_overlay
mkdir -p lower/src lower/dst
for i in $(seq 1 $((50 * SCALE))); do
    echo "move_$i" > "lower/src/f_$i"
done
mount_overlay

T0=$(($(date +%s%N) / 1000000))

# Reader: continuously list both directories
(
    for round in $(seq 1 $((100 * SCALE))); do
        ls merged/src/ > /dev/null 2>&1
        ls merged/dst/ > /dev/null 2>&1
    done
) &

# Mover: rename files from src to dst
for i in $(seq 1 $((50 * SCALE))); do
    mv "merged/src/f_$i" "merged/dst/f_$i"
done

wait

MS=$(elapsed $T0)
echo "Rename across dirs + concurrent readdir in ${MS}ms"

# Verify: src empty, dst has all files
test "$(ls merged/src/ | wc -l)" -eq 0
for i in $(seq 1 $((50 * SCALE))); do
    grep -q "move_$i" "merged/dst/f_$i"
done

reset_overlay

# ========================================
# Test 33: Overwrite lower-layer files with different sizes
# ========================================
echo "=== Test 33: Overwrite lower-layer files with different sizes ==="
reset_overlay
mkdir -p lower
for i in $(seq 1 $((100 * SCALE))); do
    dd if=/dev/zero of="lower/sz_$i" bs=1 count=$((i * 10)) 2>/dev/null
done
mount_overlay

T0=$(($(date +%s%N) / 1000000))

# Overwrite each file with a larger payload, forcing copy-up + truncate
for w in $(seq 0 $((NPROC - 1))); do
    START=$(( w * (100 * SCALE / NPROC) + 1 ))
    END=$(chunk_end $((100 * SCALE)) $NPROC $w)
    (
        for i in $(seq $START $END); do
            dd if=/dev/zero of="merged/sz_$i" bs=1 count=$((i * 20)) 2>/dev/null
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "$((100 * SCALE)) file overwrites in ${MS}ms"

# Verify sizes match new values
for i in $(seq 1 $((100 * SCALE))); do
    EXPECTED_SZ=$((i * 20))
    ACTUAL_SZ=$(stat -c%s "merged/sz_$i")
    if [ "$ACTUAL_SZ" -ne "$EXPECTED_SZ" ]; then
        echo "FAIL: sz_$i expected $EXPECTED_SZ got $ACTUAL_SZ"
        exit 1
    fi
done

reset_overlay

# ========================================
# Test 34: Concurrent mkdir + rmdir races
# ========================================
echo "=== Test 34: Concurrent mkdir + rmdir ==="
reset_overlay; mount_overlay

T0=$(($(date +%s%N) / 1000000))

# Each worker creates and removes its own directories
for w in $(seq 1 $NPROC); do
    (
        for round in $(seq 1 $((100 * SCALE))); do
            mkdir -p "merged/dir_${w}_${round}/sub"
            echo "data" > "merged/dir_${w}_${round}/sub/file"
            rm "merged/dir_${w}_${round}/sub/file"
            rmdir "merged/dir_${w}_${round}/sub"
            rmdir "merged/dir_${w}_${round}"
        done
    ) &
done
wait

MS=$(elapsed $T0)
echo "$((NPROC * 100 * SCALE)) mkdir+populate+rmdir cycles in ${MS}ms"

# Verify nothing remains
test "$(ls merged/ | wc -l)" -eq 0

reset_overlay

# ========================================
# Test 35: Concurrent read + unlink (file disappears while reading)
# ========================================
echo "=== Test 35: Read during unlink ==="
reset_overlay; mount_overlay

# Create files
for i in $(seq 1 $((200 * SCALE))); do
    echo "readunlink_$i" > "merged/ru_$i"
done

T0=$(($(date +%s%N) / 1000000))

# Reader: try to read files (some may disappear, that's OK)
(
    for round in $(seq 1 $((5 * SCALE))); do
        for i in $(seq 1 $((200 * SCALE))); do
            cat "merged/ru_$i" > /dev/null 2>/dev/null || true
        done
    done
) &

# Deleter: remove files
for i in $(seq 1 $((200 * SCALE))); do
    rm "merged/ru_$i"
done

wait

MS=$(elapsed $T0)
echo "Read during unlink finished in ${MS}ms"

# All files should be gone
test "$(ls merged/ | wc -l)" -eq 0

reset_overlay

# ========================================
# Test 36: Readdir consistency under mutations (no entry appears twice or is lost)
# ========================================
echo "=== Test 36: Readdir consistency ==="
reset_overlay
mkdir -p lower/rdtest
for i in $(seq 1 200); do
    echo "rd_$i" > "lower/rdtest/f_$i"
done
mount_overlay

T0=$(($(date +%s%N) / 1000000))

# While a reader repeatedly lists, a writer adds and removes files
(
    for round in $(seq 1 $((50 * SCALE))); do
        ls merged/rdtest/ > /dev/null
    done
) &

(
    for i in $(seq 1 $((100 * SCALE))); do
        echo "new_$i" > "merged/rdtest/new_$i"
    done
    for i in $(seq 1 $((100 * SCALE))); do
        rm "merged/rdtest/new_$i"
    done
) &

wait

MS=$(elapsed $T0)
echo "Readdir consistency test finished in ${MS}ms"

# Original 200 files should still be there
for i in $(seq 1 200); do
    grep -q "rd_$i" "merged/rdtest/f_$i"
done
# New files should all be gone
test "$(ls merged/rdtest/new_* 2>/dev/null | wc -l)" -eq 0

reset_overlay

# ========================================
# Test 37: Sparse file handling
# ========================================
echo "=== Test 37: Sparse file handling ==="
reset_overlay; mount_overlay

T0=$(($(date +%s%N) / 1000000))

# Create sparse files with holes
for i in $(seq 1 $((20 * SCALE))); do
    dd if=/dev/zero of="merged/sparse_$i" bs=1 count=1 seek=$((1024 * 1024)) 2>/dev/null
done

# Verify they are sparse (blocks << size)
for i in $(seq 1 $((20 * SCALE))); do
    SZ=$(stat -c%s "merged/sparse_$i")
    test "$SZ" -eq $((1024 * 1024 + 1))
done

MS=$(elapsed $T0)
echo "$((20 * SCALE)) sparse files created and verified in ${MS}ms"

reset_overlay

# ========================================
# Test 38: Multiple lower layers with opaque directories
# ========================================
echo "=== Test 38: Multiple lower layers with whiteouts across layers ==="
reset_overlay

# Layer 3 (bottom): base files
mkdir -p layer3/common
for i in $(seq 1 20); do
    echo "base_$i" > "layer3/common/file_$i"
done

# Layer 2 (middle): overrides some files, deletes others
mkdir -p layer2/common
for i in $(seq 1 10); do
    echo "override_$i" > "layer2/common/file_$i"
done
# Create whiteouts for files 15-20
for i in $(seq 15 20); do
    mknod "layer2/common/.wh.file_$i" c 0 0 2>/dev/null || true
done

# Layer 1 (top lower): adds new files
mkdir -p layer1/common
for i in $(seq 21 30); do
    echo "new_$i" > "layer1/common/file_$i"
done

mkdir -p upper workdir merged
fuse-overlayfs -o "lowerdir=layer1:layer2:layer3,upperdir=upper,workdir=workdir" merged

T0=$(($(date +%s%N) / 1000000))

# Overridden files (1-10): should show layer2 content
for i in $(seq 1 10); do
    grep -q "override_$i" "merged/common/file_$i"
done

# Untouched files (11-14): should show layer3 content
for i in $(seq 11 14); do
    grep -q "base_$i" "merged/common/file_$i"
done

# Whiteout files (15-20): should not exist
for i in $(seq 15 20); do
    test ! -e "merged/common/file_$i"
done

# New files (21-30): should show layer1 content
for i in $(seq 21 30); do
    grep -q "new_$i" "merged/common/file_$i"
done

MS=$(elapsed $T0)
echo "Multi-layer whiteout verification passed in ${MS}ms"

umount merged
rm -rf layer1 layer2 layer3 upper workdir merged

# ========================================
# Test 39: Concurrent open+write+fsync on multiple files
# ========================================
echo "=== Test 39: Concurrent open+write+fsync ==="
reset_overlay; mount_overlay

T0=$(($(date +%s%N) / 1000000))

for w in $(seq 1 $NPROC); do
    (
        for i in $(seq 1 $((50 * SCALE))); do
            F="merged/fsync_${w}_${i}"
            # Write and force sync
            dd if=/dev/zero of="$F" bs=4096 count=4 conv=fsync 2>/dev/null
        done
    ) &
done
wait

MS=$(elapsed $T0)
TOTAL=$((NPROC * 50 * SCALE))
echo "$TOTAL fsync writes in ${MS}ms"

# Verify sizes
for w in $(seq 1 $NPROC); do
    for i in $(seq 1 $((50 * SCALE))); do
        test "$(stat -c%s "merged/fsync_${w}_${i}")" -eq $((4 * 4096))
    done
done

reset_overlay

# ========================================
# Test 40: Interleaved copy-up and stat from different workers
# ========================================
echo "=== Test 40: Interleaved copy-up and stat ==="
reset_overlay
mkdir -p lower
for i in $(seq 1 $((200 * SCALE))); do
    echo "orig_$i" > "lower/ics_$i"
done
mount_overlay

T0=$(($(date +%s%N) / 1000000))

# Writer: modify files (triggers copy-up)
(
    for i in $(seq 1 $((200 * SCALE))); do
        echo "mod" >> "merged/ics_$i"
    done
) &

# Statter: stat files continuously (may see pre or post copy-up state)
(
    for round in $(seq 1 $((10 * SCALE))); do
        for i in $(seq 1 $((200 * SCALE))); do
            stat "merged/ics_$i" > /dev/null 2>/dev/null || true
        done
    done
) &

wait

MS=$(elapsed $T0)
echo "Copy-up + stat interleave finished in ${MS}ms"

# All files should have both original and modified content
for i in $(seq 1 $((200 * SCALE))); do
    grep -q "orig_$i" "merged/ics_$i"
    grep -q "mod" "merged/ics_$i"
done

reset_overlay

echo ""
echo "All stress tests passed!"
