#!/bin/sh
# Copyright 2018-2026 the Deno authors. MIT license.
#
# Regenerates the clips the fake capture devices send (`fake_media.rs`): one
# second of a moving test card (FFmpeg's testsrc2, 640x480 at 30 fps, one
# key frame then delta frames, no B-frames) in every video codec the engine
# packetizes, and one second of a 400 Hz tone (600 Hz on the right channel of
# the stereo clip) in every audio codec, 20 ms a frame. A one-second loop
# repeats seamlessly: the tone has whole cycles, the video restarts on its key
# frame. Needs ffmpeg with libvpx, libaom, libx264, libx265 and libopus, and
# python3. Run from this directory: `sh generate.sh`.
set -eu
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
ff="ffmpeg -hide_banner -loglevel error -y"
video="-f lavfi -i testsrc2=size=640x480:rate=30:duration=1 -pix_fmt yuv420p -g 30 -keyint_min 30 -bf 0"
$ff $video -c:v libvpx -b:v 250k -deadline realtime -cpu-used 8 \
  -auto-alt-ref 0 -lag-in-frames 0 -f ivf "$tmp/vp8.ivf"
$ff $video -c:v libvpx-vp9 -b:v 250k -deadline realtime -cpu-used 8 \
  -lag-in-frames 0 -row-mt 1 -f ivf "$tmp/vp9.ivf"
$ff $video -c:v libaom-av1 -b:v 250k -usage realtime -cpu-used 8 \
  -lag-in-frames 0 -f ivf "$tmp/av1.ivf"
$ff $video -c:v libx264 -profile:v baseline -level 3.1 -preset ultrafast \
  -tune zerolatency -b:v 250k -x264-params aud=1:repeat-headers=1 \
  -f h264 "$tmp/h264.264"
$ff $video -c:v libx265 -preset ultrafast -tune zerolatency -b:v 250k \
  -x265-params aud=1:repeat-headers=1:bframes=0:keyint=30:log-level=error \
  -f hevc "$tmp/h265.265"
mono="-f lavfi -i sine=frequency=400:sample_rate=48000:duration=1"
stereo="-f lavfi -i aevalsrc=0.125*sin(400*2*PI*t)|0.125*sin(600*2*PI*t):s=48000:d=1"
opus="-c:a libopus -b:a 32k -frame_duration 20 -vbr off -application audio"
$ff $mono -ac 1 $opus -f ogg "$tmp/opus.ogg"
$ff $stereo -ac 2 $opus -b:a 64k -f ogg "$tmp/opus-stereo.ogg"
$ff $mono -ar 8000 -ac 1 -f mulaw "$tmp/pcmu.raw"
$ff $mono -ar 8000 -ac 1 -f alaw "$tmp/pcma.raw"
$ff $mono -ar 16000 -ac 1 -c:a g722 -f g722 "$tmp/g722.raw"
python3 pack.py "$tmp" .
