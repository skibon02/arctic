[![docs-badge][]][docs] [![crates.io version]][crates.io link]
# Arctic

Rust library for offline PPI (pulse-to-pulse interval) recording with the
Polar Verity Sense optical heart rate sensor.

The Verity Sense can record PPI data to its internal memory while disconnected
from Bluetooth. This library starts and stops that recording, lists the
recorded files, and downloads and parses them.

### Note for MacOS

Using Btleplug on MacOS will require you to give your terminal (or whatever app
you're using) permissions to use Bluetooth.
View [here](https://github.com/deviceplug/btleplug#macos-permissions-note) to
see how to resolve this issue.

# Examples

There is an example in the [examples folder](https://github.com/Roughsketch/arctic/tree/main/examples)

[crates.io link]: https://crates.io/crates/arctic
[crates.io version]: https://img.shields.io/crates/v/arctic.svg?style=flat-square
[docs]: https://docs.rs/arctic
[docs-badge]: https://img.shields.io/badge/docs-online-5023dd.svg?style=flat-square
