# example-pulse

The example Rust viz plugin. Copy this folder to start your own; the rule is in
`../README.md`.

## What it shows

One row per chip: the chip's PCI address, a sparkline of its recent power draw,
and the latest power (W) and temperature (C). The sparkline is green below
`hot_c - 15`, yellow below `hot_c`, and red at or above it. A reading the chip
has not reported shows `--`. With no chips the view says `no chips`.

## Signals read (all on chip entities)

* `tt_tdp_w`: chip power in watts, sent every poll.
* `tt_asic_temperature_celsius`: ASIC temperature in degrees Celsius.

## How to enable it

It is hidden by default. Either run `tt-toplike --viz example-pulse` or add it
to the config file:

```toml
[viz]
enable = ["example-pulse"]

[viz.example-pulse]
hot_c = 85   # temperature drawn red; yellow starts 15 degrees below
```

## Files

* `mod.rs`: the plugin, commented part by part.
* `tests.rs`: unit tests and golden-frame tests.
* `golden/`: the expected frames. Regenerate with `UPDATE_GOLDEN=1`, then read
  them before committing.
