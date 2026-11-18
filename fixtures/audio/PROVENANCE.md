# Test Audio Provenance and Specifications

All audio files in this directory are 100% synthetically generated for M0 feasibility testing. No copyrighted or external commercial recordings are included.

## Generation Method

Generated using `ffmpeg` version 6.1+ via the `lavfi` (Libavfilter) synthetic `sine` audio source, encoded into standard constant bitrate (CBR) MP3 format using `libmp3lame`.

Generation script: [scripts/generate-test-audio.sh](../../scripts/generate-test-audio.sh)

## Track Manifest

| File | Frequency | Musical Note | Duration | Codec | Bitrate | Sample Rate | Channels | Purpose |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `track1_440hz.mp3` | 440.00 Hz | A4 | 5 sec | MP3 (libmp3lame) | 128 kbps CBR | 44,100 Hz | Stereo (2) | Automatic track transition test (Step 1) |
| `track2_554hz.mp3` | 554.37 Hz | C#5 | 5 sec | MP3 (libmp3lame) | 128 kbps CBR | 44,100 Hz | Stereo (2) | Automatic track transition test (Step 2) |
| `track3_659hz.mp3` | 659.25 Hz | E5 | 5 sec | MP3 (libmp3lame) | 128 kbps CBR | 44,100 Hz | Stereo (2) | Automatic track transition test (Step 3) |
| `track4_880hz.mp3` | 880.00 Hz | A5 | 5 sec | MP3 (libmp3lame) | 128 kbps CBR | 44,100 Hz | Stereo (2) | Automatic track transition test (Step 4) |
| `track_long_30min.mp3` | 440.00 Hz | A4 | 1800 sec (30 min) | MP3 (libmp3lame) | 128 kbps CBR | 44,100 Hz | Stereo (2) | Continuous 30-min playback & 10-min lock screen endurance test |

## Provenance and Licensing

- **Author**: Synthetic wave generation by FFmpeg libavfilter
- **License**: Creative Commons Zero (CC0) / Public Domain Dedication
- **Integrity Verification**: Inspectable and reproducible at any time via `scripts/generate-test-audio.sh`.
