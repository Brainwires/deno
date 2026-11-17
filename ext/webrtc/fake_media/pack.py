# Copyright 2018-2026 the Deno authors. MIT license.
"""Cuts generate.sh's encoder output into frames and writes each clip as
`<u32 little-endian length><frame>` records, the format fake_media.rs reads."""
import os
import struct
import sys

VIDEO_FRAMES = 30  # one second at 30 fps
AUDIO_FRAMES = 50  # one second of 20 ms frames


def ivf(path):
    data = open(path, "rb").read()
    assert data[:4] == b"DKIF", path
    pos = struct.unpack_from("<H", data, 6)[0]
    frames = []
    while pos < len(data):
        size = struct.unpack_from("<I", data, pos)[0]
        pos += 12
        frames.append(data[pos:pos + size])
        pos += size
    return frames


def annexb(path, is_aud):
    """Access units of an Annex B stream, each starting at its AUD."""
    data = open(path, "rb").read()
    starts = []
    i = 0
    while True:
        i = data.find(b"\x00\x00\x01", i)
        if i < 0:
            break
        begin = i - 1 if i > 0 and data[i - 1] == 0 else i
        if is_aud(data[i + 3]):
            starts.append(begin)
        i += 3
    starts.append(len(data))
    return [data[a:b] for a, b in zip(starts, starts[1:])]


def ogg_packets(path):
    data = open(path, "rb").read()
    pos = 0
    packets = []
    partial = b""
    while pos < len(data):
        assert data[pos:pos + 4] == b"OggS", path
        nsegs = data[pos + 26]
        table = data[pos + 27:pos + 27 + nsegs]
        body = pos + 27 + nsegs
        for lacing in table:
            partial += data[body:body + lacing]
            body += lacing
            if lacing < 255:
                packets.append(partial)
                partial = b""
        pos = body
    # OpusHead and OpusTags come first.
    return packets[2:]


def fixed(path, size):
    data = open(path, "rb").read()
    return [data[i:i + size] for i in range(0, len(data) - size + 1, size)]


def write(out, name, frames, count):
    assert len(frames) >= count, (name, len(frames))
    with open(os.path.join(out, name + ".bin"), "wb") as f:
        for frame in frames[:count]:
            assert frame
            f.write(struct.pack("<I", len(frame)))
            f.write(frame)


def main():
    src, out = sys.argv[1], sys.argv[2]
    j = lambda name: os.path.join(src, name)
    write(out, "vp8", ivf(j("vp8.ivf")), VIDEO_FRAMES)
    write(out, "vp9", ivf(j("vp9.ivf")), VIDEO_FRAMES)
    write(out, "av1", ivf(j("av1.ivf")), VIDEO_FRAMES)
    write(out, "h264", annexb(j("h264.264"), lambda b: b & 0x1F == 9),
          VIDEO_FRAMES)
    write(out, "h265", annexb(j("h265.265"), lambda b: (b >> 1) & 0x3F == 35),
          VIDEO_FRAMES)
    write(out, "opus", ogg_packets(j("opus.ogg")), AUDIO_FRAMES)
    write(out, "opus-stereo", ogg_packets(j("opus-stereo.ogg")), AUDIO_FRAMES)
    # 20 ms at 8000 samples a second, one byte a sample (G.722: 64 kbit/s).
    write(out, "pcmu", fixed(j("pcmu.raw"), 160), AUDIO_FRAMES)
    write(out, "pcma", fixed(j("pcma.raw"), 160), AUDIO_FRAMES)
    write(out, "g722", fixed(j("g722.raw"), 160), AUDIO_FRAMES)


main()
