# Write-Ahead Log (WAL)

This document describes the Write-Ahead Log design in Persisting.

## Overview

Write-Ahead Log (WAL) ensures data durability by recording changes before they are applied to the main storage. This allows recovery of uncommitted data after crashes.

## Design Goals

1. **Durability** - No data loss after acknowledged writes
2. **Performance** - Minimal overhead for write operations
3. **Recovery** - Fast and correct crash recovery
4. **Simplicity** - Easy to understand and maintain

## Architecture

```
Write Request
     │
     ▼
┌─────────────────────────────────────────┐
│            WAL Writer                   │
│                                         │
│  1. Serialize record to bytes           │
│  2. Write length + checksum + data      │
│  3. fsync() (if sync mode)              │
│                                         │
└────────────────┬────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────┐
│            WAL File                     │
│                                         │
│  ┌─────────────────────────────────┐   │
│  │ Record 1: [len][crc][data]      │   │
│  ├─────────────────────────────────┤   │
│  │ Record 2: [len][crc][data]      │   │
│  ├─────────────────────────────────┤   │
│  │ Record 3: [len][crc][data]      │   │
│  └─────────────────────────────────┘   │
│                                         │
└────────────────┬────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────┐
│          Memory Buffer                  │
│                                         │
│  (Records buffered for batch flush)     │
│                                         │
└────────────────┬────────────────────────┘
                 │
                 ▼ flush()
┌─────────────────────────────────────────┐
│          Lance Dataset                  │
│                                         │
│  (Durable columnar storage)             │
│                                         │
└────────────────┬────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────┐
│          Truncate WAL                   │
│                                         │
│  (Remove committed records)             │
│                                         │
└─────────────────────────────────────────┘
```

## Record Format

Each WAL record has the following format:

```
┌────────────┬────────────┬────────────────────┐
│  Length    │  Checksum  │       Data         │
│  (4 bytes) │  (4 bytes) │    (variable)      │
└────────────┴────────────┴────────────────────┘
```

- **Length**: Record data length (uint32, little-endian)
- **Checksum**: CRC32 of data (uint32, little-endian)
- **Data**: Serialized record (JSON or MessagePack)

## Write Path

### Normal Write

```python
async def put(self, record: dict) -> None:
    if self.enable_wal:
        # 1. Write to WAL first
        await self._wal.append(record)
    
    # 2. Add to memory buffer
    async with self._condition:
        self._buffer.append(record)
        self._condition.notify_all()
```

### Sync Modes

| Mode | Description | Durability | Performance |
|------|-------------|------------|-------------|
| `sync` | fsync after each write | Highest | Slowest |
| `batch` | fsync periodically | High | Fast |
| `async` | OS-controlled sync | Low | Fastest |

```python
# Sync every write (safest)
backend_options={"wal_sync_interval": 0.0}

# Sync every 100ms (balanced)
backend_options={"wal_sync_interval": 0.1}

# OS-controlled (fastest)
backend_options={"wal_sync_interval": 1.0}
```

## Recovery

### Startup Recovery

```python
async def _recover(self) -> None:
    """Recover uncommitted records from WAL."""
    if not self._wal_path.exists():
        return
    
    recovered = []
    async for record in self._wal.read_all():
        recovered.append(record)
    
    if recovered:
        logger.info(f"Recovered {len(recovered)} records from WAL")
        self._buffer.extend(recovered)
```

### Recovery Flow

```
Startup
    │
    ▼
┌─────────────────────────────────────────┐
│          Check WAL exists               │
│                                         │
│  WAL path: {storage_path}/wal/          │
│  WAL file: {bucket_id}.wal              │
│                                         │
└────────────────┬────────────────────────┘
                 │
        ┌────────┴────────┐
        │                 │
   WAL exists        No WAL
        │                 │
        ▼                 ▼
┌───────────────┐  ┌───────────────┐
│ Read records  │  │ Normal start  │
│ Validate CRC  │  │               │
│ Add to buffer │  │               │
└───────┬───────┘  └───────────────┘
        │
        ▼
┌───────────────┐
│ Continue with │
│ recovery data │
└───────────────┘
```

### Corruption Handling

```python
async def _read_record(self, file) -> dict | None:
    """Read and validate a single record."""
    try:
        # Read length
        length_bytes = await file.read(4)
        if len(length_bytes) < 4:
            return None
        length = struct.unpack('<I', length_bytes)[0]
        
        # Read checksum
        checksum_bytes = await file.read(4)
        checksum = struct.unpack('<I', checksum_bytes)[0]
        
        # Read data
        data = await file.read(length)
        
        # Validate checksum
        if crc32(data) != checksum:
            logger.warning("WAL record corruption detected")
            return None
        
        return json.loads(data)
    except Exception as e:
        logger.error(f"WAL read error: {e}")
        return None
```

## Truncation

WAL is truncated after successful flush:

```python
async def flush(self) -> None:
    """Flush buffer to Lance and truncate WAL."""
    async with self._condition:
        if not self._buffer:
            return
        
        # 1. Write to Lance
        await self._write_to_lance(self._buffer)
        
        # 2. Truncate WAL (records are now durable)
        if self.enable_wal:
            await self._wal.truncate()
        
        # 3. Clear buffer
        self._buffer.clear()
```

## File Management

### WAL Directory Structure

```
{storage_path}/
└── {topic}/
    └── bucket_{id}/
        ├── data.lance/          # Lance dataset
        └── wal/
            ├── 0.wal           # Current WAL file
            └── 0.wal.tmp       # Temp file during rotation
```

### WAL Rotation

When WAL exceeds `max_wal_size`:

```python
async def _maybe_rotate(self) -> None:
    """Rotate WAL if it exceeds max size."""
    if self._wal_size > self.max_wal_size:
        # 1. Force flush to Lance
        await self.flush()
        
        # 2. Create new WAL file
        await self._wal.rotate()
        
        self._wal_size = 0
```

## Configuration

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `enable_wal` | bool | False | Enable WAL |
| `wal_sync_interval` | float | 1.0 | Sync interval (0=every write) |
| `max_wal_size` | int | 100MB | Max WAL size before rotation |

## Performance Considerations

### Write Amplification

Each write goes to both WAL and buffer:
- WAL: Sequential append (fast)
- Buffer: In-memory (fast)
- Lance: Batch write on flush (efficient)

### Sync Trade-offs

```
                    Durability
                        ▲
                        │
        sync mode ──────┤
                        │
       batch mode ──────┤
                        │
        async mode ─────┤
                        │
                        └──────────────────► Performance
```

### Best Practices

1. **Use batch sync for production**: Balance durability and performance
2. **Size WAL appropriately**: Too small = frequent rotation, too large = slow recovery
3. **Monitor WAL size**: Alert on unexpected growth
4. **Test recovery**: Regularly validate recovery works

## Limitations

1. **Single-writer assumption**: WAL assumes one writer per bucket
2. **In-order recovery**: Records are recovered in write order
3. **No partial record recovery**: Corrupted records are skipped

## Future Improvements

1. **Compression**: Compress WAL records
2. **Async I/O**: Use io_uring on Linux
3. **Group commit**: Batch sync across multiple writes
4. **Checksummed chunks**: Handle partial writes better

