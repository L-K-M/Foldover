#!/usr/bin/env python3
"""Synthesize the fold's paper sounds into apple/Foldover/Sounds.

Pure standard library, fixed seeds: the same WAV bytes on every run.

    python3 tools/sounds/make_fold_sounds.py

Each sound is a few layers of shaped noise and sine, mono 44.1 kHz:

    Swish  the leaf moving through air   swept band-passed noise, 240 ms
    Press  the leaf landing on the sheet  70 Hz thump + noise slap, 180 ms
    Peel   the leaf lifting off again     crackle impulses in noise, 300 ms
    Tick   ink changing direction         short bright click, 45 ms
"""

import math
import random
import struct
import wave
from pathlib import Path

RATE = 44_100
OUT = Path(__file__).resolve().parents[2] / "apple" / "Foldover" / "Sounds"
# Peak level after normalising: quiet enough to sit under system sounds.
PEAK = 0.5


def samples(seconds):
    return int(RATE * seconds)


def envelope(n, attack, curve=3.0):
    """Linear attack, then a power-curve decay to silence."""
    a = max(1, samples(attack))
    out = []
    for i in range(n):
        if i < a:
            out.append(i / a)
        else:
            out.append((1 - (i - a) / max(1, n - a)) ** curve)
    return out


def noise(n, seed):
    rng = random.Random(seed)
    return [rng.uniform(-1, 1) for _ in range(n)]


def lowpass(signal, cutoff):
    """One-pole low-pass; `cutoff` may be a number or a per-sample list."""
    out, y = [], 0.0
    for i, x in enumerate(signal):
        c = cutoff[i] if isinstance(cutoff, list) else cutoff
        alpha = 1 - math.exp(-2 * math.pi * c / RATE)
        y += alpha * (x - y)
        out.append(y)
    return out


def highpass(signal, cutoff):
    low = lowpass(signal, cutoff)
    return [x - l for x, l in zip(signal, low)]


def bandpass(signal, low, high):
    return lowpass(highpass(signal, low), high)


def mix(*layers):
    n = max(len(layer) for layer in layers)
    return [sum(layer[i] for layer in layers if i < len(layer)) for i in range(n)]


def scale(signal, gains):
    if isinstance(gains, (int, float)):
        return [x * gains for x in signal]
    return [x * g for x, g in zip(signal, gains)]


def write(name, signal):
    peak = max(abs(x) for x in signal) or 1
    data = b"".join(struct.pack("<h", int(max(-1, min(1, x / peak * PEAK)) * 32_767)) for x in signal)
    OUT.mkdir(parents=True, exist_ok=True)
    with wave.open(str(OUT / f"{name}.wav"), "wb") as file:
        file.setnchannels(1)
        file.setsampwidth(2)
        file.setframerate(RATE)
        file.writeframes(data)


def swish():
    n = samples(0.24)
    # The band rises as the leaf speeds up, then falls as it slows.
    sweep = [600 + 2_400 * math.sin(math.pi * i / n) for i in range(n)]
    air = lowpass(highpass(noise(n, 1), 300), sweep)
    return scale(air, envelope(n, 0.09, curve=2.0))


def press():
    n = samples(0.18)
    thump = [math.sin(2 * math.pi * 70 * i / RATE) for i in range(n)]
    thump = scale(thump, envelope(n, 0.004, curve=4.0))
    slap = bandpass(noise(n, 2), 200, 2_500)
    slap = scale(slap, envelope(n, 0.002, curve=9.0))
    return mix(scale(thump, 0.9), scale(slap, 0.7))


def peel():
    n = samples(0.30)
    rng = random.Random(3)
    crackle = [0.0] * n
    # Fibres letting go: sparse impulses, densest in the middle.
    for i in range(n):
        density = 0.012 * math.sin(math.pi * i / n)
        if rng.random() < density:
            crackle[i] = rng.uniform(0.4, 1.0) * rng.choice((-1, 1))
    crackle = bandpass(crackle, 1_500, 7_000)
    hiss = scale(bandpass(noise(n, 4), 800, 4_000), 0.25)
    body = mix(scale(crackle, 3.0), hiss)
    return scale(body, envelope(n, 0.05, curve=1.6))


def tick():
    n = samples(0.045)
    blip = [math.sin(2 * math.pi * 2_200 * i / RATE) for i in range(n)]
    click = highpass(noise(n, 5), 3_000)
    body = mix(scale(blip, 0.6), scale(click, 0.5))
    return scale(body, envelope(n, 0.001, curve=6.0))


if __name__ == "__main__":
    write("FoldSwish", swish())
    write("FoldPress", press())
    write("FoldPeel", peel())
    write("FoldTick", tick())
