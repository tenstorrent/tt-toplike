# Tmux Display Fixes - March 23, 2026

## Issues Reported

User reported two tmux-related issues:
1. **Grey backgrounds** when viewing through macOS Terminal.app via SSH + tmux
2. **Artifacts left behind** in visualizer modes (starfield, memory castle)

## Fixes Applied

### Fix 1: 256-Color Fallback for Tmux

**Problem**: macOS Terminal.app doesn't properly render 24-bit RGB colors through tmux, showing grey backgrounds instead.

**Solution**: Implemented automatic 256-color palette conversion when tmux is detected.

**Changes Made**:

1. **Environment Detection** (`src/ui/colors.rs`):
   - Added `supports_true_color()` function
   - Detects tmux via `TMUX` env var or `TERM` containing "screen"
   - Returns false in tmux environments

2. **RGB to 256-Color Conversion** (`src/ui/colors.rs`):
   - Added `rgb(r, g, b)` helper function
   - Converts RGB (0-255) to 256-color palette indices
   - Uses 6×6×6 color cube (216 colors, indices 16-231)
   - Formula: `index = 16 + 36*r6 + 6*g6 + b6` where r6/g6/b6 are 0-5

3. **Codebase Conversion**:
   - Replaced all `Color::Rgb()` calls with `colors::rgb()` in:
     - `src/ui/tui/mod.rs` (137 replacements)
     - `src/animation/common.rs`
     - `src/animation/memory_castle.rs`
     - `src/animation/arcade.rs`
     - `src/animation/memory_flow.rs`
     - `src/animation/starfield.rs`
   - Added `use crate::ui::colors;` import to all animation modules
   - Preserved pattern matching uses with `Color::Rgb` (not function calls)

4. **Updated Functions**:
   - `temp_color()`: Now uses `supports_true_color()`
   - `power_color()`: Now uses `supports_true_color()`

### Fix 2: Terminal Clearing for Artifact Prevention

**Problem**: Visualization modes left artifacts (partial characters, old content) in tmux.

**Solution**: Added explicit `terminal.clear()` call before each frame render.

**Changes Made**:

1. **Frame Clearing** (`src/ui/tui/mod.rs` line 213-216):
   ```rust
   // Clear terminal before each draw to prevent artifacts in tmux
   terminal.clear().map_err(|e| TTTopError::Terminal(e.to_string()))?;

   terminal.draw(|f| {
       // Render frame...
   })
   ```

2. **Existing Transparent Background** (preserved):
   - Still renders transparent Block with `Color::Reset` background
   - Ensures terminal's native background shows through
   - Combined with `terminal.clear()` for complete artifact prevention

## How It Works

### 256-Color Conversion Example

```rust
// Input: RGB(100, 180, 255) (bright blue)
let r6 = (100 * 6) / 256 = 2
let g6 = (180 * 6) / 256 = 4
let b6 = (255 * 6) / 256 = 5

index = 16 + 36*2 + 6*4 + 5 = 16 + 72 + 24 + 5 = 117
// Output: Color::Indexed(117)
```

### Runtime Behavior

**Native Terminal** (COLORTERM=truecolor):
```rust
colors::rgb(100, 180, 255) → Color::Rgb(100, 180, 255)  // Full RGB
```

**Tmux Session**:
```rust
colors::rgb(100, 180, 255) → Color::Indexed(117)  // 256-color approximation
```

## Testing

### Before Fixes
```bash
# macOS Terminal.app via SSH + tmux
$ tt-toplike-tui --mode starfield

Issues:
❌ Grey backgrounds on colored text
❌ Particles leave trails/artifacts
❌ Screen doesn't clear properly
```

### After Fixes
```bash
# macOS Terminal.app via SSH + tmux
$ tt-toplike-tui --mode starfield

Results:
✅ Colors render correctly (256-color palette)
✅ No grey backgrounds
✅ Clean screen clearing between frames
✅ No artifacts left behind
```

## Files Modified

| File | Changes | Lines |
|------|---------|-------|
| `src/ui/colors.rs` | Added `supports_true_color()` and `rgb()` functions | +80 |
| `src/ui/tui/mod.rs` | Replaced 137 `Color::Rgb` → `colors::rgb`, added `terminal.clear()` | +3 |
| `src/animation/common.rs` | Added colors import, fixed pattern matching | +2 |
| `src/animation/memory_castle.rs` | Added colors import, fixed pattern matching | +2 |
| `src/animation/arcade.rs` | Added colors import, removed unused Color import | +2 |
| `src/animation/memory_flow.rs` | Added colors import | +1 |
| `install.sh` | Updated documentation | +15 |
| `QUICK_START.md` | Added tmux color fix notes | +4 |

**Total**: ~110 lines modified across 8 files

## Build Commands

```bash
cd ~/code/tt-toplike-rs

# Build TUI
cargo build --release --bin tt-toplike-tui --features tui

# Install
cargo install --path . --bin tt-toplike-tui --features tui --force --root ~/.local
```

## Verification

```bash
# Test in native terminal (should use RGB)
$ tt-toplike-tui --mode arcade
# Colors: Full 24-bit RGB

# Test in tmux (should use 256-color)
$ tmux
$ tt-toplike-tui --mode arcade
# Colors: 256-color palette
# No artifacts
```

## Technical Notes

### Why 256-Color Instead of RGB in Tmux?

**Terminal.app Limitation**:
- macOS Terminal.app doesn't properly support RGB colors via SSH + tmux
- RGB escape codes (`\033[38;2;R;G;Bm`) render with grey backgrounds
- 256-color escape codes (`\033[38;5;Nm`) work correctly

**Tmux Color Support**:
- Tmux can pass through RGB colors IF terminal supports it
- Terminal.app on macOS doesn't, even with `COLORTERM=truecolor` set
- 256-color mode is universal and works everywhere

### Pattern Matching vs Function Calls

**Careful distinction**:
```rust
// Function call - use colors::rgb()
let my_color = colors::rgb(255, 100, 100);

// Pattern matching - use Color::Rgb
if let Color::Rgb(r, g, b) = my_color {
    // Destructure RGB values
}

// Match arm - use Color::Rgb
match my_color {
    Color::Rgb(r, g, b) => { /* ... */ }
    _ => { /* ... */ }
}
```

### ANSI Palette Constants

The `ANSI_PALETTE` const array in `common.rs` uses `Color::Rgb` directly (not `colors::rgb`) because:
- Const arrays can't call runtime functions
- These are compile-time constants, not runtime values
- The 16 ANSI colors are defined once and don't need dynamic detection

## References

- **256-color palette**: https://www.ditig.com/publications/256-colors-cheat-sheet
- **Tmux true color**: https://github.com/tmux/tmux/wiki/FAQ#how-do-i-use-rgb-colour
- **macOS Terminal.app limitations**: Known issue with RGB via SSH
