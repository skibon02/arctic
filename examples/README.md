# Examples

Each example can be run from this directory using `cargo run -p <PROJECT-NAME>`.

## `offline-ppi`

Starts offline PPI recording on a Polar Verity Sense, then lists and downloads
the recorded data. Pass the device ID as a command line argument.

## `discover-record`

Scans for the first Polar Verity Sense and ensures an offline PPI recording is
running: starts one if none is active, or stops the existing one and reports the
result. No device ID is required.

## `list-recordings`

Connects to a Polar Verity Sense by device ID and prints the available offline
recordings. Pass the device ID as a command line argument.
