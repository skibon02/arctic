## Offline PPI recording (Verity Sense)

- [x] Connect to Verity Sense and discover services
- [x] Start offline PPI recording over PMD control point
- [x] Stop offline PPI recording
- [x] List offline recordings via PS-FTP (PMDFILES.TXT)
- [x] Download offline recordings via PS-FTP (RFC76 framing + protobuf)
- [x] Parse offline PPI file (header, start time, PPI samples)
- [x] XOR encryption support (none + xor)

## Online streaming (Verity Sense)

- [x] Shared notification dispatcher and disconnect handling
- [x] Request and select stream settings (sample rate, resolution, range, channels)
- [x] Stream PPI samples (raw frames)
- [x] Stream ACC samples (raw and delta frames, factor scaling)
- [x] Stream GYRO samples (delta frames)
- [x] Stream MAG samples (delta frames, calibration status)
- [x] Per-sample timestamps for ACC, GYRO, and MAG
- [x] Stop on drop, device-initiated stop, and disconnect
- [x] Concurrent streams of multiple types (fan-out on PMD data, independent stop)

## Future work

- [ ] AES-128/256 encryption
- [ ] Offline recording triggers (exercise/system start)
- [ ] Offline recording parsing for ACC, GYRO, MAG, PPG
- [ ] PPG online streaming
- [ ] PPI timestamps (the device sends PPI frames with a zero timestamp)
