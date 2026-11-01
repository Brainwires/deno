#!/usr/bin/env python3
# The input devices of the E2E_DISPLAY=wayland session (linux/session.sh): one
# zwp_virtual_keyboard_v1 and one zwlr_virtual_pointer_v1 that last the whole
# run, the keyboard driven over a Unix socket.
#
# wlroots' headless backend has no input device, so without this the seat has no
# keyboard and no pointer, as no real seat is: no client ever has keyboard focus,
# the clipboard's selection events (sent only to the focused client) never come,
# and GTK's drag code meets a seat with no pointer. A keyboard is also how the
# suite presses keys. `wtype` would do neither: it makes a new device with a
# keymap holding only the keys it types, for the length of one press, and GTK's
# accelerators don't fire across that swap (keyval and state arrive right, the
# accelerator does not match). This one uploads the plain "us" layout once, as a
# real keyboard has, and keeps it.
#
# It speaks the Wayland wire protocol directly: python3 is already needed here
# (notification-server.py) and no distro packages a client for this protocol.
# libxkbcommon, which every host here links, makes the keymap.
#
# usage: wayland-input.py <control socket>
# A control connection sends one line of keys ("ctrl+shift+k"), and gets "ok" or
# "error: ..." back once the press and release are sent.
import array
import ctypes
import os
import select
import socket
import struct
import sys
import time

KEYS = {"F9": 67, "k": 37, "Escape": 1, "Down": 108, "Return": 28}
MODS = {  # the evdev code, and the mask in the "us" keymap's real modifiers.
    "shift": (42, 1),
    "ctrl": (29, 4),
    "alt": (56, 8),
    "super": (125, 64),
}
REMOTE_KEYMAP_XKB_V1 = 1


def keymap_text():
    x = ctypes.CDLL("libxkbcommon.so.0")
    x.xkb_context_new.restype = ctypes.c_void_p
    x.xkb_keymap_new_from_names.restype = ctypes.c_void_p
    x.xkb_keymap_new_from_names.argtypes = [
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.c_int,
    ]
    x.xkb_keymap_get_as_string.restype = ctypes.c_void_p
    x.xkb_keymap_get_as_string.argtypes = [ctypes.c_void_p, ctypes.c_int]
    ctx = x.xkb_context_new(0)
    # An empty xkb_rule_names: the defaults, "evdev" / "pc105" / "us".
    names = (ctypes.c_char_p * 5)()
    km = x.xkb_keymap_new_from_names(ctx, ctypes.addressof(names), 0)
    if not km:
        raise RuntimeError("libxkbcommon could not compile the us keymap")
    return ctypes.string_at(x.xkb_keymap_get_as_string(km, 1))


def string_arg(s):
    b = s.encode() + b"\0"
    return struct.pack("=I", len(b)) + b + b"\0" * (-len(b) % 4)


class Wayland:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.next_id = 2
        self.buf = b""

    def new_id(self):
        i = self.next_id
        self.next_id += 1
        return i

    def send(self, obj, op, payload=b"", fds=()):
        msg = struct.pack("=II", obj, ((8 + len(payload)) << 16) | op) + payload
        anc = []
        if fds:
            anc = [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", fds))]
        self.sock.sendmsg([msg], anc)

    def events(self):
        """Read what is there; yields (object, opcode, payload)."""
        self.buf += self.sock.recv(65536)
        while len(self.buf) >= 8:
            obj, word = struct.unpack("=II", self.buf[:8])
            size = word >> 16
            if len(self.buf) < size:
                return
            yield obj, word & 0xffff, self.buf[8:size]
            self.buf = self.buf[size:]

    def drain(self):
        for obj, op, payload in self.events():
            if obj == 1 and op == 0:  # wl_display.error
                o, code = struct.unpack("=II", payload[:8])
                raise RuntimeError(f"wl_display.error on {o}: code {code}")


def connect():
    runtime = os.environ["XDG_RUNTIME_DIR"]
    w = Wayland(os.path.join(runtime, os.environ["WAYLAND_DISPLAY"]))
    registry = w.new_id()
    w.send(1, 1, struct.pack("=I", registry))  # wl_display.get_registry
    done = w.new_id()
    w.send(1, 0, struct.pack("=I", done))  # wl_display.sync
    globals_ = {}
    finished = False
    while not finished:
        for obj, op, p in w.events():
            if obj == 1 and op == 0:
                raise RuntimeError("wl_display.error during setup")
            if obj == registry and op == 0:  # wl_registry.global
                name, n = struct.unpack("=II", p[:8])
                iface = p[8:8 + n - 1].decode()
                ver = struct.unpack("=I", p[8 + n + (-n % 4):][:4])[0]
                globals_[iface] = (name, ver)
            elif obj == done:
                finished = True

    def bind(iface, version):
        if iface not in globals_:
            raise RuntimeError(f"the compositor has no {iface}")
        i = w.new_id()
        name = globals_[iface][0]
        # wl_registry.bind: the name, then a new_id with its interface + version.
        w.send(
            registry,
            0,
            struct.pack("=I", name) + string_arg(iface) +
            struct.pack("=II", version, i),
        )
        return i

    seat = bind("wl_seat", 1)
    manager = bind("zwp_virtual_keyboard_manager_v1", 1)
    kbd = w.new_id()
    # create_virtual_keyboard(seat, id)
    w.send(manager, 0, struct.pack("=II", seat, kbd))
    text = keymap_text() + b"\0"
    fd = os.memfd_create("e2e-keymap")
    os.write(fd, text)
    os.lseek(fd, 0, os.SEEK_SET)
    # keymap(format, fd, size): the fd goes in ancillary data, not the payload.
    w.send(kbd, 0, struct.pack("=II", REMOTE_KEYMAP_XKB_V1, len(text)), [fd])
    os.close(fd)
    # A pointer, resting mid-screen. create_virtual_pointer(seat, id)
    pointers = bind("zwlr_virtual_pointer_manager_v1", 1)
    pointer = w.new_id()
    w.send(pointers, 0, struct.pack("=II", seat, pointer))
    # motion_absolute(time, x, y, x_extent, y_extent), then frame.
    w.send(pointer, 1, struct.pack("=IIIII", 0, 1, 1, 2, 2))
    w.send(pointer, 4)
    return w, kbd


def press(w, kbd, spec):
    parts = spec.split("+")
    mods = parts[:-1]
    key = parts[-1]
    if key not in KEYS or any(m not in MODS for m in mods):
        raise ValueError(f"unknown key in {spec}")
    now = lambda: int(time.monotonic() * 1000) & 0xffffffff

    def key_event(code, state):
        w.send(kbd, 1, struct.pack("=III", now(), code, state))

    # A virtual keyboard's key does not move the modifier state: it is sent
    # separately, as the protocol's modifiers request (depressed, latched,
    # locked, group).
    mask = 0
    for m in mods:
        code, bit = MODS[m]
        key_event(code, 1)
        mask |= bit
        w.send(kbd, 2, struct.pack("=IIII", mask, 0, 0, 0))
    key_event(KEYS[key], 1)
    time.sleep(0.03)
    key_event(KEYS[key], 0)
    for m in reversed(mods):
        code, bit = MODS[m]
        key_event(code, 0)
        mask &= ~bit
        w.send(kbd, 2, struct.pack("=IIII", mask, 0, 0, 0))


def main():
    control = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    control.bind(sys.argv[1])
    control.listen(8)
    w, kbd = connect()
    print("ready", flush=True)
    while True:
        ready, _, _ = select.select([control, w.sock], [], [])
        if w.sock in ready:
            w.drain()
        if control in ready:
            c, _ = control.accept()
            with c:
                c.settimeout(5)
                try:
                    press(w, kbd, c.recv(256).decode().strip())
                    c.sendall(b"ok\n")
                except Exception as e:
                    c.sendall(f"error: {e}\n".encode())


main()
