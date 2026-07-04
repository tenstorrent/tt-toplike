# Process Monitoring

tt-toplike detects which processes are using Tenstorrent hardware by scanning `/proc/[pid]/fd/` for open device file descriptors and checking memory mappings for hugepages.

## What It Shows

The **Insights screen** (the default view) carries a **process panel** alongside the per-chip portraits. It lists processes by resource use and tags any that match a known inference runtime (ollama, vLLM, llama.cpp, MLX, ComfyUI, …); on TT hardware it additionally attributes per-process device usage and serving metrics. (The old "Hardware Usage" box that lived under the retired Normal-mode telemetry table is gone — its detection logic now feeds this panel.)

```
╔══[ PROCESSES ]══▓▒░
║ Device 0                                            
║   python3 [42315]  vllm.entrypoints.openai...  ⟶ vLLM
║      (hugepages: 8 × 1GB)
║ Device 1
║   python3 [42316]  vllm.worker --model...       ⟶ vLLM
║   … and 3 more
╚══════════════════════════════════════════════
```

Navigate the list with `↑`/`↓`; `k` silences the selected process's alerts and `K` kills it (Linux). The panel is populated only when processes are detected.

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
- The Insights process panel shows up to 12 rows: inference-matched processes are always kept, and the remaining slots are filled by the busiest processes

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
