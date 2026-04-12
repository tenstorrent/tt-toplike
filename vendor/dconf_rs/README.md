# dconf_rs

[![Build Status](https://travis-ci.org/kylecorry31/dconf_rs.svg?branch=master)](https://travis-ci.org/kylecorry31/dconf_rs)

A Rust API for interacting with dconf.

Version 0.3.0 - [Changelog](CHANGELOG.md)

## Getting started
### Dependencies
- rustc
- cargo
- dconf

#### Build from sources
```Shell
cargo build --release
```

### Usage
First add the following to your `Cargo.toml`:
```toml
[dependencies]
dconf_rs = "0.2.1"
```

Next, add this to your crate root:
```rust
extern crate dconf_rs;
```


#### Examples
```rust
// later
dconf_rs::set_boolean("/path/to/dconf/key", false);
```

## Contributing
Please fork this repo and submit a pull request to contribute. I will review all changes and respond if they are accepted or rejected (as well as reasons, so it will be accepted).

### Issues
If you are submitting a bug, please describe the bug in detail and how to replicate if possible. Logs are also very useful.

If you are submitting a feature idea, please describe it in detail and document the potential use cases of that feature if it isn't clear.

## Credits
- [@kylecorry31](https://github.com/kylecorry31) - Initial work

## License
You are free to copy, modify, and distribute do-not-disturb with attribution under the terms of the MIT license. See the [LICENSE](LICENSE) file for details.
