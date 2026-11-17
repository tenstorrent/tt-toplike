# Rust viz plugins

A plugin is one folder here. Add a folder and rebuild; no other file changes.
`build.rs` finds the folder and registers the plugin when `viz::init()` runs.

## The rule

The folder `src/viz/plugins/<snake_name>/` must contain a `mod.rs` that has:

```rust
/// Whether the view is on in the view cycle before the user chooses.
pub const ENABLED_BY_DEFAULT: bool = true;

/// Build one instance of the plugin.
pub fn make() -> Box<dyn crate::viz::Viz> { Box::new(MyViz::new()) }
```

* The folder name is snake_case: lowercase letters, digits and `_`, starting
  with a letter. The build stops with a message if it is not.
* The id returned by `make().id()` must be the folder name with each `_`
  replaced by `-`. The folder `heat_map` has the id `heat-map`. A test checks
  this for every folder.
* Put the rest of the plugin (extra files, helpers, golden test frames) in
  the same folder. Declare extra files with `mod` lines inside your `mod.rs`.

## Examples

* `example_pulse/` is the smallest one: reads the world and history, draws, keeps no state.
* `game_of_life/` keeps state between frames: it steps on the bus clock, reacts to chip power,
  resizes without losing its board and uses its own seeded random generator. Copy it when the
  view animates or accumulates. The section "A plugin that keeps state" in
  `docs/viz-plugins.md` explains the pattern.

## Testing

Use `crate::viz::testkit` to render the plugin to text and compare it with a
golden file kept in your folder.

## Packaging

Everything under `src/viz/plugins/` ships in the crate, because `Cargo.toml`
has no `include` list that leaves it out.
