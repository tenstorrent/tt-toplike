# SSH Color Support Fix

## Problem

When connecting via SSH from macOS, the starfield visualization appears in black and white instead of full color, while the Memory Castle visualization displays colors correctly.

## Root Cause

The terminal's true color (24-bit RGB) capability is not being communicated through the SSH session. The `COLORTERM` environment variable, which tells applications that the terminal supports 24-bit color, is not forwarded by default through SSH.

## Solution 1: Enable COLORTERM in SSH (Recommended)

### On your Mac (SSH client):

**Option A: Add to SSH config**
```bash
# Edit ~/.ssh/config
Host *
    SendEnv COLORTERM
```

**Option B: Set before connecting**
```bash
export COLORTERM=truecolor
ssh user@host
```

**Option C: One-liner**
```bash
COLORTERM=truecolor ssh user@host
```

### On the server (if needed):

Ensure SSH accepts the COLORTERM variable:
```bash
# Edit /etc/ssh/sshd_config (requires sudo)
AcceptEnv COLORTERM

# Restart SSH daemon
sudo systemctl restart sshd
```

## Solution 2: Automatic Fallback (Now Built-in!)

As of this commit, tt-toplike-rs automatically detects if true color is supported and falls back to 256-color palette when needed:

- **With COLORTERM=truecolor**: Full 24-bit RGB colors (smooth gradients)
- **Without COLORTERM**: 256-color indexed palette (still colorful!)

The starfield will now show color even in limited terminals, though with less smooth gradients.

## Testing

Test color support in your terminal:
```bash
# Check environment
echo "TERM=$TERM"
echo "COLORTERM=$COLORTERM"
tput colors  # Should show 256 or more

# Test with different settings
COLORTERM=truecolor ./tt-toplike-tui --mock --mock-devices 2  # Full RGB
unset COLORTERM && ./tt-toplike-tui --mock --mock-devices 2   # 256-color fallback
```

## Why Memory Castle Worked

The Memory Castle visualization uses high-contrast, saturated RGB values that Ratatui can successfully approximate using the 256-color palette even without true color support. The starfield's subtle temperature gradients required the fallback logic to work properly.

## Technical Details

**Color Mapping (256-color fallback):**
- Temperature colors: Cyan (51) → Yellow (226) → Orange (214) → Red (196)
- Power colors: Cyan (51) → Blue (75) → Orange (214) → Red (196)

**Code Changes:**
- Modified `src/ui/colors.rs`: Added `COLORTERM` detection
- Functions affected: `temp_color()`, `power_color()`
- Tests updated to handle both RGB and indexed colors
