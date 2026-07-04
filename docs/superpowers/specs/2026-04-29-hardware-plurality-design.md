# Design: Hardware Plurality — Multi-Distro + Board/Chip Topology Fix

**Issues**: #9 (Ubuntu 22.04 support), #10 (Board 0/1 wrong for single-chip cards)

---

## Issue #9: Multi-distro .deb packages

**Root cause**: Binary compiled on ubuntu-24.04 links against glibc 2.39; Ubuntu 22.04 ships glibc 2.35.

**Fix**: Add a parallel matrix job in `release.yml` that runs on `ubuntu-22.04`.
- Noble job: runs as-is on ubuntu-24.04 → `libc6 >= 2.39`
- Jammy job: patches `debian/changelog` suite field `noble → jammy` via sed; builds on ubuntu-22.04 → `libc6 >= 2.35` (also installable on 24.04)
- Both jobs upload independently to the same GitHub Release
- Output files renamed with suite suffix before upload so they coexist: `tt-toplike_0.4.x_amd64_noble.deb` / `..._jammy.deb`

No changelog entry added for jammy — CI patches it transiently at build time.

---

## Issue #10: Board vs Chip topology

**Root cause**: `BoardTopology::from_devices()` hard-codes `chips_per_board = 2`, so 4× p150a (single-chip PCIe cards) are grouped into "Board 0 → [Dev0, Dev1]" and "Board 1 → [Dev2, Dev3]" — but p150a has one chip per card.

### Auto-detect chips_per_board from board_type

- `p150*`, `n150*`, `e75*`, `e150*` → `chips_per_board = 1` (single-chip cards)
- `p300*`, `n300*` → `chips_per_board = 2` (dual-chip carrier boards)
- Unknown/mixed → `chips_per_board = 2` (conservative)

When all devices are single-chip: `has_multi_chip_boards() = false`. Visual consequences:
- **Board-label row suppressed** in Memory Castle (labels only meaningful when boards span ≥2 chips)
- **Topology diagram** uses `·` spacer instead of `═══`/`←→` (no intra-card links for standalone PCIe cards)

### Scale plan: up to 128+ chips

| Chip count | Memory Castle view |
|-----------|-------------------|
| ≤ `width/20` | Side-by-side columns (current behaviour) |
| > `width/20` | Fleet grid (compact 2-row cells) |

**Fleet grid**: Dynamic column count = `max(1, min(4, (width-4)/40))`.
- 80-col terminal → 1 col (32 rows for 32 chips)
- 160-col terminal → 3 cols (11 rows for 32 chips)
- Each cell: `Dev  0 BH ████████░░░░ 16W 43°C` (~35 chars)

**Arcade topology diagram**: Detailed for ≤ 8 chips. Compact mini-bar for > 8:
- `32× BH  [░▒▓█░▒░░▒▓█░▒▓█░▒░▓█░░▒▓█░░▒▓]` — 1 char per chip, color = temp, char = power

**Side-by-side cap** removed; auto-calculated from terminal width at runtime.

---

## Files changed

| File | Change |
|------|--------|
| `src/animation/topology.rs` | `is_single_chip_card()`, fix `from_devices()`, add `has_multi_chip_boards()`, update tests |
| `src/animation/memory_castle.rs` | Board-label condition → `has_multi_chip_boards()`; dynamic cols; fleet grid |
| `src/animation/arcade.rs` | `topology_diagram_line` → compact mini-bar for > 8 chips |
| `.github/workflows/release.yml` | Matrix: noble + jammy; rename + upload |
