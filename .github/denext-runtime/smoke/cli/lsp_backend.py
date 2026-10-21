#!/usr/bin/env python3
"""Start `deno lsp`, initialize it as an editor with no settings would, shut
it down, and print what it logged (stderr): which TypeScript backend the
session picked (DENEXT.md item 94).

Usage: lsp_backend.py <deno> <workspace dir>
"""

import json
import pathlib
import subprocess
import sys
import threading


def frame(message):
    body = json.dumps(message).encode()
    return b"Content-Length: %d\r\n\r\n" % len(body) + body


def read_message(stream):
    length = None
    while True:
        line = stream.readline()
        if not line:
            raise SystemExit("deno lsp closed its output")
        line = line.strip()
        if not line:
            break
        name, _, value = line.decode().partition(":")
        if name.lower() == "content-length":
            length = int(value)
    return json.loads(stream.read(length))


def wait_for(stream, request_id):
    while True:
        message = read_message(stream)
        if message.get("id") == request_id and "method" not in message:
            return message


def main():
    deno, workspace = sys.argv[1], pathlib.Path(sys.argv[2]).resolve()
    proc = subprocess.Popen(
        [deno, "lsp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=workspace,
    )
    stderr = []
    reader = threading.Thread(target=lambda: stderr.append(proc.stderr.read()))
    reader.start()
    send = lambda m: (proc.stdin.write(frame(m)), proc.stdin.flush())
    send({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "processId": None,
            "rootUri": workspace.as_uri(),
            "capabilities": {},
        },
    })
    wait_for(proc.stdout, 1)
    send({"jsonrpc": "2.0", "method": "initialized", "params": {}})
    send({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": None})
    wait_for(proc.stdout, 2)
    send({"jsonrpc": "2.0", "method": "exit", "params": None})
    proc.stdin.close()
    proc.wait(timeout=60)
    reader.join(timeout=60)
    sys.stdout.write(b"".join(stderr).decode(errors="replace"))


if __name__ == "__main__":
    main()
