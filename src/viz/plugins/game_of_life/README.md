# game-of-life

A Rust viz plugin that keeps state between frames: Conway's Game of Life, fed by the chips.

## What it shows

One status row (`gen`, `alive`, how many chips feed it) and a board below it, two cells per
terminal row. The board is split into one vertical band per chip, in chip order. The rules are the
standard ones (B3/S23) on a torus, so a glider that leaves one edge returns on the other.

## What reacts

* **Power.** Each chip's power draw is compared with the lowest and highest it has drawn since the
  view opened (the range relaxes slowly toward the present value). The nearer the top of that range,
  the more new live cells fall into the chip's band every generation. A chip at rest adds nothing,
  so its band settles into still lifes and blinkers.
* **A sharp rise in power** (more than 15% of the range in one frame) drops a glider into the band.
* **Temperature.** Cells are green below `hot_c - 15`, yellow below `hot_c`, red at or above it.
* A chip with no power reading, or a range narrower than 5 W, adds nothing.

One generation runs every 125 ms of the bus clock, at most four per frame. The random numbers come
from a fixed-seed generator, so the same readings at the same times draw the same board.

## Signals read (chip entities)

* `tt_tdp_w`: live power draw in watts.
* `tt_asic_temperature_celsius`: ASIC temperature in degrees Celsius.

## Enable it

```
tt-toplike --mock 4 --viz game-of-life
```

or in `~/.config/tt-toplike/config.toml`:

```toml
[viz]
enable = ["game-of-life"]

[viz.game-of-life]
hot_c = 85
```

Needs a terminal at least 20 x 6.
