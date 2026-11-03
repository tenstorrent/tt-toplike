# Process Monitoring

tt-toplike detects which processes are using Tenstorrent hardware from `/proc`: the driver's holder lists, open device file descriptors, and memory mappings for hugepages. The first scan runs on a worker thread as the TUI starts, so the answer is on screen a few hundred milliseconds after launch, before `tt-smi` has answered.

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

- **Driver holder lists**: Reads `/proc/driver/tenstorrent/<N>/pids`, where tt-kmd lists every pid holding device `N` open. The file is world readable, so this covers other users, root and processes in containers. It is the same file `tt-smi` reads for its `processes[]`.
- **Device files**: Reads the `/proc/[pid]/fd/` links of processes this user owns and may read, for `/dev/tenstorrent/*` targets. Links are read with `readlink`; nothing is opened or traced. At most 16,384 links per process, 32,768 processes per scan, and 4,096 pids or 64 KiB per driver list are read; past the process bound the scan stops and counts the rest.
- **Hugepages**: Reads `/proc/[pid]/maps` for 1GB (`hugepages-1G`, `pagesize-1GB`) and 2MB hugepage entries
- **Shared resources**: Catches processes with Tenstorrent-related memory mappings but no specific device file

## Device numbers and device indices

`/dev/tenstorrent/<N>` is the driver's number for a card. The backends number devices by PCI address. tt-toplike maps one to the other through `/sys/class/tenstorrent/tenstorrent!<N>/device`, which names the card's PCI address, so a process is shown on the right chip even when the driver numbers the cards in another order. When no number maps (no class directory), each number is used as the index itself. When some map, a number that does not is left out, so two cards never share an index.

## Blending with tt-smi

When `tt-smi`'s snapshot arrives, its `processes[]` joins what `/proc` already showed (see `src/workload/device_holders.rs`):

- `/proc` stays authoritative. When the driver's holder lists were read, `/proc` saw every holder and `tt-smi` adds nothing.
- Without the holder lists (an older tt-kmd), `tt-smi` fills in the processes `/proc` could not read.
- A process seen by both is one entry, with the devices `/proc` saw.
- Nothing `/proc` showed is removed when `tt-smi` arrives; an entry leaves when a later scan no longer sees it.
- `tt-smi -s` itself holds every device while it reads them. The Wafer Waffler does not treat it as a process event.

## Platform and Build

Requires Linux with procfs. Enabled by the `linux-procfs` feature (on by default).

```bash
# With process monitoring (default)
cargo build --bin tt-toplike-tui --features tui

# Without process monitoring
cargo build --bin tt-toplike-tui --no-default-features --features tui,json-backend
```

## Performance

- The scan runs on a worker thread every 2 seconds (independent of telemetry refresh); the first one starts as the TUI starts
- About 150 to 300 ms on a box with 550 processes, longer under load; the render thread never waits for it
- Processes of other users are never read beyond their world-readable `stat` and `cmdline`; a refused `fd/` directory is skipped and counted

## Limitations

- Without the driver's holder lists (older tt-kmd), non-root users see only their own processes
- Hugepage and mapping detection reads `/proc/[pid]/maps`, so it covers only this user's processes
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
