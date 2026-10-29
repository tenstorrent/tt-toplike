#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.
"""Generate src/animation/die3d/bh_layout_data.rs from real Blackhole silicon.

Run with the tt-exalens venv, while holding a gozer lease on the board(s):

    gozer acquire --chips 1 --who claude:tt-toplike --reason "die layout (board A)"
    export TT_VISIBLE_DEVICES=...            # as printed by gozer
    ~/.venvs/tt-exalens-latest/bin/python scripts/gen_die_layout.py \
        --dump board_a.json --label 'board A'
    gozer release <lease>                    # then repeat for the next board
    ~/.venvs/tt-exalens-latest/bin/python scripts/gen_die_layout.py \
        --merge board_a.json board_b.json --out src/animation/die3d/bh_layout_data.rs

Why two steps: a lease covers one board (two chips) at a time, so each board is
dumped to JSON separately and the dumps are merged offline.

The script only enumerates tiles (`get_block_locations`, `loc.to("die")`,
`loc.to("noc0")`). It never reads a tile register, so it does not touch any
tile NIU. Harvesting removes different tiles on each chip, so the layout is the
union of the tile lists of all chips.

`--merge ... --report-harvest` prints, per chip, the `enabled_tensix_col` mask from
`tt-smi -s` and the die columns that have no Tensix tile in exalens' list.
"""
import argparse
import datetime
import json
import subprocess
import sys

# exalens block name -> (kind code, kind name). Codes match TileKind::from_code
# in layout.rs: 0 Tensix, 1 Dram, 2 Eth, 3 Mgmt (arc, pcie, l2cpu, security).
BLOCKS = {
    "functional_workers": 0,
    "dram": 1,
    "eth": 2,
    "arc": 3,
    "pcie": 3,
    "l2cpu": 3,
    "security": 3,
}


def enumerate_chips():
    """Return one record per chip: {"index", "tiles": [[dx,dy,nx,ny,kind,block]]}."""
    # Imported late: creating the context opens /dev/tenstorrent/*.
    from ttexalens.tt_exalens_init import init_ttexalens

    ctx = init_ttexalens()
    chips = []
    for idx, dev in enumerate(ctx.devices.values() if hasattr(ctx.devices, "values") else ctx.devices):
        tiles = []
        for block, kind in BLOCKS.items():
            for loc in dev.get_block_locations(block):
                die = loc.to("die")
                noc0 = loc.to("noc0")
                tiles.append([die[0], die[1], noc0[0], noc0[1], kind, block])
        chips.append({"index": idx, "tiles": tiles})
    return chips


def exalens_version():
    try:
        from importlib.metadata import version

        return version("tt-exalens")
    except Exception:
        return "unknown"


def smi_masks():
    """enabled_tensix_col per visible chip, from `tt-smi -s`, in device order.

    The field lives under `smbus_telem` in the snapshot. tt-smi opens the
    device, so this also needs the lease.
    """
    out = subprocess.run(["tt-smi", "-s"], capture_output=True, text=True, check=True).stdout
    snap = json.loads(out[out.index("{"):])
    return [int(dev["smbus_telem"]["ENABLED_TENSIX_COL"], 16) for dev in snap["device_info"]]


def report_harvest(dumps):
    """Print, per chip, the Tensix mask and the die columns with no Tensix tile.

    "Missing" is measured against every die column that has a Tensix tile on
    at least one chip. Columns harvested on every chip are listed separately,
    since no tile list contains them.
    """
    chips = [(d["label"], c, m) for d in dumps for c, m in zip(d["chips"], d["masks"])]
    per_chip = [{t[0] for t in c["tiles"] if t[4] == 0} for _, c, _ in chips]
    seen = set().union(*per_chip)
    print("board                              chip  enabled_tensix_col  missing die columns")
    for (label, c, mask), have in zip(chips, per_chip):
        print(f"{label:<34} {c['index']:>4}  {mask:#018x}  {sorted(seen - have)}")
    print(f"Tensix die columns seen on some chip: {sorted(seen)}")


def fold(d, n):
    """Die index -> noc0 index (0, 1, n-1, 2, n-2, ...); same rule as layout.rs."""
    if d == 0:
        return 0
    return (d + 1) // 2 if d % 2 else n - d // 2


def fill_harvest_holes(union, n_cols, n_rows):
    """Fill die columns that are harvested on every dumped chip.

    A column inside the span of Tensix columns that has no Tensix tile in the
    union gets Tensix tiles at the die rows where the other Tensix columns
    have them. Its noc0 coordinate is the fold of its die coordinate, which
    is the rule every measured tile obeys. Returns the filled (die x, die y)
    cells, sorted.
    """
    tensix = [(x, y) for (x, y), (_, _, k) in union.items() if k == 0]
    xs = {x for x, _ in tensix}
    rows = sorted({y for _, y in tensix})
    filled = []
    for col in range(min(xs), max(xs) + 1):
        if col in xs:
            continue
        for y in rows:
            if (col, y) in union:
                sys.exit(f"hole column {col} already has a non-Tensix tile at row {y}")
            union[(col, y)] = (fold(col, n_cols), fold(y, n_rows), 0)
            filled.append((col, y))
    return sorted(filled)


def write_rust(dumps, out_path):
    union = {}
    sources = []
    for d in dumps:
        sources.append(d)
        for chip in d["chips"]:
            for dx, dy, nx, ny, kind, _block in chip["tiles"]:
                key = (dx, dy)
                val = (nx, ny, kind)
                if union.setdefault(key, val) != val:
                    sys.exit(f"chips disagree at die {key}: {union[key]} vs {val}")
    n_cols = max(k[0] for k in union) + 1
    n_rows = max(k[1] for k in union) + 1
    filled = fill_harvest_holes(union, n_cols, n_rows)
    n_chips = sum(len(d["chips"]) for d in dumps)
    versions = sorted({d.get("exalens") for d in dumps})
    boards = ", ".join(d.get("label", "?") for d in dumps)
    lines = [
        "// SPDX-License-Identifier: Apache-2.0",
        "// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.",
        "",
        "//! GENERATED by scripts/gen_die_layout.py. Do not edit by hand.",
        "//!",
        f"//! Generated {datetime.date.today().isoformat()} with tt-exalens {', '.join(map(str, versions))},",
        f"//! from {n_chips} Blackhole chips ({boards}, chips 0 and 1 only). Tile lists differ per chip",
        "//! because of harvesting, so this is their union over the full die grid.",
        "//!",
        f"//! Harvest holes filled by pattern, not measured (Tensix, noc0 = fold of die): {filled or 'none'}.",
        "//!",
        "//! Each row is (die x, die y, noc0 x, noc0 y, kind code); kind codes are",
        "//! 0 Tensix, 1 Dram, 2 Eth, 3 Mgmt (arc, pcie, l2cpu, security).",
        "",
        "pub const BH_TILES: &[(u8, u8, u8, u8, u8)] = &[",
    ]
    for (dx, dy), (nx, ny, k) in sorted(union.items(), key=lambda kv: (kv[0][1], kv[0][0])):
        lines.append(f"    ({dx}, {dy}, {nx}, {ny}, {k}),")
    lines.append("];")
    with open(out_path, "w") as f:
        f.write("\n".join(lines) + "\n")
    cols = max(k[0] for k in union) + 1
    rows = max(k[1] for k in union) + 1
    counts = {}
    for _, _, k in union.values():
        counts[k] = counts.get(k, 0) + 1
    print(f"wrote {out_path}: {len(union)} tiles, grid {cols}x{rows}, kind counts {counts}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dump", metavar="JSON", help="enumerate the visible chips and write a per-board JSON dump")
    ap.add_argument("--label", default="board", help="name for this dump in the generated header")
    ap.add_argument("--merge", nargs="+", metavar="JSON", help="merge dumps into the Rust table")
    ap.add_argument("--out", default="src/animation/die3d/bh_layout_data.rs", help="Rust output path for --merge")
    ap.add_argument("--report-harvest", action="store_true",
                    help="with --merge: print enabled_tensix_col and the missing Tensix die columns per chip")
    args = ap.parse_args()

    if args.dump:
        chips = enumerate_chips()
        with open(args.dump, "w") as f:
            json.dump({"label": args.label, "exalens": exalens_version(), "chips": chips,
                       "masks": smi_masks()}, f)
        print(f"{len(chips)} chips: " + ", ".join(f"chip {c['index']}: {len(c['tiles'])} tiles" for c in chips))
    elif args.merge:
        dumps = [json.load(open(p)) for p in args.merge]
        if args.report_harvest:
            report_harvest(dumps)
        write_rust(dumps, args.out)
    else:
        ap.error("give --dump or --merge")


if __name__ == "__main__":
    main()
