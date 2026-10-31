"""Kokoro speech for agent voice output, run by the API as a short-lived subprocess.

Reads {"text": ..., "voice": ...} as JSON on stdin; the API has already prepared and bounded the
text and validated the voice. Writes length-prefixed frames to stdout (a 4-byte big-endian length,
then the bytes): first a JSON plan {"chunks": [characters per chunk]}, sent before the model loads
so a player can size its track at once, then one MP3 per chunk, so playback can start before the
whole reply is synthesized. No network access is used.
"""
import io
import json
import os
import re
import shutil
import struct
import sys
import types

import phonemizer.backend.espeak.api as espeak_api
import soundfile
from kokoro_onnx import Kokoro

# phonemizer loads a private copy of libespeak-ng from a temporary directory, but the API
# container's writable mounts are noexec. One espeak instance per process needs no copy, so link
# to the read-only original instead.
espeak_api.shutil = types.SimpleNamespace(
    copy=lambda source, target, follow_symlinks=True: os.symlink(source, target),
    rmtree=shutil.rmtree,
)

MODELS = "/opt/voice/models"
FIRST_CHUNK_CHARS = 120  # short first chunk: audio starts after one sentence
CHUNK_CHARS = 400


def chunks(text):
    """Sentence groups: the first at most FIRST_CHUNK_CHARS (unless one sentence is longer)."""
    group, limit = "", FIRST_CHUNK_CHARS
    for sentence in re.split(r"(?<=[.!?;:])\s+", text):
        if group and len(group) + len(sentence) > limit:
            yield group
            group, limit = "", CHUNK_CHARS
        group = (group + " " + sentence).strip()
    if group:
        yield group


def frame(out, data):
    out.write(struct.pack(">I", len(data)) + data)
    out.flush()


def main():
    os.nice(10)  # speech uses every core; the API and loop keep priority
    request = json.load(sys.stdin)
    voice = request["voice"]
    parts = list(chunks(request["text"]))
    out = sys.stdout.buffer
    frame(out, json.dumps({"chunks": [len(text) for text in parts]}).encode())
    kokoro = Kokoro(MODELS + "/kokoro-v1.0.int8.onnx", MODELS + "/voices-v1.0.bin")
    lang = "en-gb" if voice.startswith("b") else "en-us"
    for text in parts:
        audio, rate = kokoro.create(text, voice=voice, lang=lang)
        mp3 = io.BytesIO()
        soundfile.write(mp3, audio, rate, format="MP3")
        frame(out, mp3.getvalue())


if __name__ == "__main__":
    main()
