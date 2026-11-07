#!/usr/bin/env python3
"""Framed SFTP counter for isolated performance fixtures, without changing data.

Usage: sftp-relay.py --log events.jsonl [--delay-ms N] -- SERVER [ARGS...]
Delay is per response frame, not a model of network RTT. Use an ssh command as
SERVER for real-network runs, or sftp-server for local protocol experiments.
"""

import argparse
import json
import os
import struct
import subprocess
import sys
import threading
import time


def packet(stream):
    head = stream.read(4)
    if not head:
        return None
    if len(head) != 4:
        raise EOFError("partial SFTP frame header")
    size = struct.unpack(">I", head)[0]
    if size > 16 * 1024 * 1024:
        raise ValueError("oversized SFTP frame")
    body = stream.read(size)
    if len(body) != size:
        raise EOFError("partial SFTP frame body")
    return head + body


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--log", required=True)
    parser.add_argument("--delay-ms", type=float, default=0)
    parser.add_argument("server", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.server[1:] if args.server[:1] == ["--"] else args.server
    if not command or args.delay_ms < 0:
        parser.error("a server command and nonnegative delay are required")
    child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    lock = threading.Lock()
    outstanding = set()
    started = time.monotonic_ns()
    with open(args.log, "a", buffering=1) as log:
        def record(direction, frame):
            body = frame[4:]
            kind = body[0]
            ident = struct.unpack(">I", body[1:5])[0] if len(body) >= 5 else None
            with lock:
                if kind not in (1, 2):
                    if direction == "request":
                        outstanding.add(ident)
                    else:
                        outstanding.discard(ident)
                event = dict(event=direction, kind=kind, id=ident, bytes=len(frame),
                             outstanding=len(outstanding), pid=os.getpid(),
                             since_ns=time.monotonic_ns() - started)
                if kind == 103 and len(body) >= 9:
                    event["data_bytes"] = struct.unpack(">I", body[5:9])[0]
                log.write(json.dumps(event, sort_keys=True) + "\n")

        def requests():
            try:
                while (frame := packet(sys.stdin.buffer)) is not None:
                    record("request", frame)
                    child.stdin.write(frame)
                    child.stdin.flush()
            finally:
                child.stdin.close()

        worker = threading.Thread(target=requests, daemon=True)
        worker.start()
        try:
            while (frame := packet(child.stdout)) is not None:
                if args.delay_ms:
                    time.sleep(args.delay_ms / 1000)
                record("response", frame)
                sys.stdout.buffer.write(frame)
                sys.stdout.buffer.flush()
        finally:
            if child.poll() is None:
                child.terminate()
            child.wait()


if __name__ == "__main__":
    main()
