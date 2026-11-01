"""Kroko streaming speech recognition for composer dictation, run by the API per dictation.

Reads raw 16 kHz mono 16-bit little-endian PCM on stdin for as long as the dictation lasts and
writes one JSON line {"text": ...} to stdout whenever the transcript so far changes; the text is
already punctuated and cased. At end of input it flushes the model and writes the final line,
then exits. The model loads before stdin is first read, so a spare process can wait warm. No
network access is used.
"""
import json
import select
import sys

import numpy
import sherpa_onnx

MODEL = "/opt/voice/models/kroko"
RATE = 16000
# A dictation that stops sending audio this long has been abandoned by its client.
IDLE_SECONDS = 60
# Silence appended at end of input so the model emits the last word.
TAIL = numpy.zeros(RATE * 2 // 3, dtype=numpy.float32)

recognizer = sherpa_onnx.OnlineRecognizer.from_transducer(
    tokens=f"{MODEL}/tokens.txt",
    encoder=f"{MODEL}/encoder.onnx",
    decoder=f"{MODEL}/decoder.onnx",
    joiner=f"{MODEL}/joiner.onnx",
    num_threads=1,
    sample_rate=RATE,
    feature_dim=80,
    decoding_method="greedy_search",
)
stream = recognizer.create_stream()
stdin = sys.stdin.buffer
last = None


def emit():
    global last
    while recognizer.is_ready(stream):
        recognizer.decode_stream(stream)
    text = recognizer.get_result(stream).strip()
    if text != last:
        last = text
        print(json.dumps({"text": text}), flush=True)


started = False
pending = b""
while True:
    # A warm spare waits indefinitely for its first audio; an active dictation does not.
    if started and not select.select([stdin], [], [], IDLE_SECONDS)[0]:
        sys.exit(1)
    data = stdin.read1(65536)
    if not data:
        break
    started = True
    data = pending + data
    usable = len(data) - len(data) % 2
    pending = data[usable:]
    samples = numpy.frombuffer(data[:usable], dtype="<i2").astype(numpy.float32) / 32768
    stream.accept_waveform(RATE, samples)
    emit()
stream.accept_waveform(RATE, TAIL)
stream.input_finished()
emit()
if last is None:
    print(json.dumps({"text": ""}), flush=True)
