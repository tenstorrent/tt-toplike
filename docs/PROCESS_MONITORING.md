# Process Monitoring

tt-toplike-rs detects which processes are using Tenstorrent hardware by scanning `/proc/[pid]/fd/` for open device file descriptors and checking memory mappings for hugepages.

## What It Shows

In Normal mode, a **Hardware Usage** section appears below the telemetry table when any processes are active:

```
┌─────────────────────────────────────────────────────┐
│ 🔧 Hardware Usage                                   │
│ Device 0: 1 process                                 │
│   └─ python3 [42315] vllm.entrypoints.openai...    │
│      (hugepages: 8 x 1GB)                          │
│ Device 1: 2 processes                               │
│   ├─ python3 [42316] vllm.worker --model...        │
│   └─ and 3 more...                                  │
└─────────────────────────────────────────────────────┘
```

The section is hidden when no processes are detected, preserving space for the device table.

## Detection Methods

- **Device files**: Scans `/proc/[pid]/fd/` for open `/dev/tenstorrent/*` descriptors
- **Hugepages**: Reads `/proc/[pid]/maps` for 1GB (`hugepages-1G`, `pagesize-1GB`) and 2MB hugepage entries
- **Shared resources**: Catches processes with Tenstorrent-related memory mappings but no specific device file

## Platform and Build

Requires Linux with procfs. Enabled by the `linux-procfs` feature (on by default).

```bash
# With process monitoring (default)
cargo build --bin tt-toplike-tui --features tui

# Without process monitoring
cargo build --bin tt-toplike-tui --no-default-features --features tui,json-backend
```

## Performance

- Scan runs every 2 seconds (independent of telemetry refresh)
- Typically completes in <10ms
- Silently skips processes where `/proc` access is denied

## Limitations

- Non-root users may not see all processes
- Shows the container host process (e.g., `containerd-shim`), not processes inside containers
- Displays up to 2 processes per device to avoid UI bloat

## Manual Inspection

Find processes using Tenstorrent devices directly:

```bash
for pid in /proc/[0-9]*; do
    if ls -l $pid/fd 2>/dev/null | grep -q tenstorrent; then
        echo "PID: $(basename $pid)"
        cat $pid/cmdline 2>/dev/null | tr '\0' ' '
        echo ""
    fi
done
```
