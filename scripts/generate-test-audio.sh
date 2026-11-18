#!/bin/sh
# Generate synthetic test audio files for M0 feasibility testing.
set -eu

SCRIPT_DIR=$(dirname "$0")
OUTPUT_DIR="${1:-$SCRIPT_DIR/../fixtures/audio}"
mkdir -p "$OUTPUT_DIR"

command -v ffmpeg >/dev/null 2>&1 || {
    echo "ERROR: ffmpeg is required to generate test audio files" >&2
    exit 1
}

echo "Generating M0 synthetic audio fixtures into $OUTPUT_DIR..."

# Short track 1: 440 Hz (A4), 5 seconds, 128 kbps CBR MP3 (~ -6.5 dBFS peak)
ffmpeg -y -v warning -f lavfi -i "sine=frequency=440:duration=5" \
    -af "volume=15dB" -c:a libmp3lame -b:a 128k -ar 44100 -ac 2 \
    "$OUTPUT_DIR/track1_440hz.mp3"

# Short track 2: 554.37 Hz (C#5), 5 seconds, 128 kbps CBR MP3 (~ -6.5 dBFS peak)
ffmpeg -y -v warning -f lavfi -i "sine=frequency=554.37:duration=5" \
    -af "volume=15dB" -c:a libmp3lame -b:a 128k -ar 44100 -ac 2 \
    "$OUTPUT_DIR/track2_554hz.mp3"

# Short track 3: 659.25 Hz (E5), 5 seconds, 128 kbps CBR MP3 (~ -6.5 dBFS peak)
ffmpeg -y -v warning -f lavfi -i "sine=frequency=659.25:duration=5" \
    -af "volume=15dB" -c:a libmp3lame -b:a 128k -ar 44100 -ac 2 \
    "$OUTPUT_DIR/track3_659hz.mp3"

# Short track 4: 880 Hz (A5), 5 seconds, 128 kbps CBR MP3 (~ -6.5 dBFS peak)
ffmpeg -y -v warning -f lavfi -i "sine=frequency=880:duration=5" \
    -af "volume=15dB" -c:a libmp3lame -b:a 128k -ar 44100 -ac 2 \
    "$OUTPUT_DIR/track4_880hz.mp3"

# Long track: 1800 seconds (30 minutes), 440 Hz clear tone, 128 kbps CBR MP3 (~ -9.5 dBFS peak)
ffmpeg -y -v warning -f lavfi -i "sine=frequency=440:duration=1800" \
    -af "volume=12dB" -c:a libmp3lame -b:a 128k -ar 44100 -ac 2 \
    "$OUTPUT_DIR/track_long_30min.mp3"

echo "Audio generation complete:"
ls -lh "$OUTPUT_DIR"/*.mp3
