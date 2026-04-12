# Changelog - dconf_rs

## v0.3.0 - October 20, 2018

**Changed:**
- All commands return Result instead of Option
  - Error messages are now easy to understand

## v0.2.1 - October 19, 2018

**Bug Fixes:**
- Removed unused variable

## v0.2.0 - October 19, 2018

**Changed:**
- All commands return Option instead of Result
- Write changed to set, read changed to get

## v0.1.0 - October 18, 2018

Initial release.

**Added:**
- Basic dconf support
  - Read and write str, bool, i32, u32, and f64.
