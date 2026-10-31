"""Kokoro speech for agent voice output, run by the API as a short-lived subprocess.

Reads {"text": ..., "voice": ...} as JSON on stdin; the API has already prepared and bounded the
text and validated the voice. Writes MP3 chunks to stdout, each as a 4-byte big-endian length then
the bytes, so playback can start before the whole reply is synthesized. No network access is used.
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


def main():
    request = json.load(sys.stdin)
    voice = request["voice"]
    kokoro = Kokoro(MODELS + "/kokoro-v1.0.int8.onnx", MODELS + "/voices-v1.0.bin")
    lang = "en-gb" if voice.startswith("b") else "en-us"
    out = sys.stdout.buffer
    for text in chunks(request["text"]):
        audio, rate = kokoro.create(text, voice=voice, lang=lang)
        mp3 = io.BytesIO()
        soundfile.write(mp3, audio, rate, format="MP3")
        out.write(struct.pack(">I", mp3.tell()) + mp3.getvalue())
        out.flush()


if __name__ == "__main__":
    main()
