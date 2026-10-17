"""sasha's speaker: keeps a voice model loaded and says what it's sent.

sasha writes this file beside the voice models and starts it on first
use: `python speaker.py SOCKET VOICE_DIR`. Each connection sends one line
of JSON, either
  {"say": [{"text": "...", "tone": "pleased"}, ...], "voice": "bm_fable",
   "engine": "chatterbox"}
or {"stop": true}. Something new to say cuts off what it was saying.

Kokoro reads plainly in any of its voices. Chatterbox speaks with
feeling: it copies the voice from a reference recording (Kokoro reading
a passage, made once and kept in VOICE_DIR) and gives each part its
tone. Parts play one after another while the next is being made. The
speaker leaves after a while with nothing to say.
"""

import json
import os
import queue
import socket
import subprocess
import sys
import tempfile
import threading
import time
import traceback
import wave

import numpy as np
from mlx_audio.tts.utils import load_model

SOCKET = sys.argv[1]
VOICE_DIR = sys.argv[2]
KOKORO = os.environ.get("SASHA_KOKORO_MODEL", "mlx-community/Kokoro-82M-bf16")
CHATTERBOX = os.environ.get("SASHA_CHATTERBOX_MODEL", "mlx-community/Chatterbox-TTS-fp16")
PLAYER = os.environ.get("SASHA_PLAYER", "afplay")
IDLE = 20 * 60
PAUSE = 0.35

# How much feeling each tone gets: Chatterbox's exaggeration.
TONES = {"calm": 0.3, "warm": 0.45, "pleased": 0.6, "concerned": 0.65, "excited": 0.9, "urgent": 0.95}
# What Kokoro reads for Chatterbox to copy a voice from: varied, unhurried.
REFERENCE = (
    "Here is where things stand. Most of the work is done, and the tests are passing. "
    "One session still needs a decision from you, and I think it's a good one to make today. "
    "Take your time; I'll keep an eye on everything else."
)

models = {}
loading = threading.Lock()
lock = threading.Lock()
# Which request is current, and the player playing it.
current = {"id": 0, "player": None}
last_used = time.time()
generating = threading.Lock()


def model(name):
    with loading:
        if name not in models:
            models[name] = load_model(KOKORO if name == "kokoro" else CHATTERBOX)
        return models[name]


def reference(voice):
    """A recording of `voice` for Chatterbox to copy, made once by Kokoro."""
    path = os.path.join(VOICE_DIR, f"{voice}.wav")
    if not os.path.exists(path):
        kokoro = model("kokoro")
        audio = np.concatenate(
            [np.array(r.audio).reshape(-1) for r in kokoro.generate(text=REFERENCE, voice=voice, lang_code=voice[0])]
        )
        write_wav(audio, kokoro.sample_rate, path)
    return path


def stop():
    with lock:
        current["id"] += 1
        player = current["player"]
        current["player"] = None
    if player and player.poll() is None:
        player.kill()


def still(mine):
    with lock:
        return current["id"] == mine


def say(parts, voice, engine):
    stop()
    with lock:
        mine = current["id"]
    clips = queue.Queue()
    threading.Thread(target=play, args=(clips, mine), daemon=True).start()
    # One generation at a time; a newer request makes this one's moot.
    with generating:
        try:
            for part in parts:
                if not still(mine):
                    break
                text = part.get("text", "").strip()
                if text:
                    clips.put(make(text, part.get("tone") or "warm", voice, engine))
        except Exception:
            traceback.print_exc()
        finally:
            clips.put(None)


def make(text, tone, voice, engine):
    """One part, as a wav file with a short pause after it."""
    if engine == "chatterbox":
        try:
            box = model("chatterbox")
            feeling = TONES.get(tone, 0.45)
            results = box.generate(
                text=text, ref_audio=reference(voice), exaggeration=feeling, cfg_weight=0.3 if feeling > 0.6 else 0.5
            )
            return to_file(results, box.sample_rate)
        except Exception:
            # Chatterbox can't: Kokoro reads it plainly instead.
            traceback.print_exc()
    kokoro = model("kokoro")
    return to_file(kokoro.generate(text=text, voice=voice, lang_code=voice[0]), kokoro.sample_rate)


def to_file(results, rate):
    audio = np.concatenate([np.array(r.audio).reshape(-1) for r in results])
    audio = np.concatenate([audio, np.zeros(int(rate * PAUSE), dtype=np.float32)])
    handle, path = tempfile.mkstemp(suffix=".wav")
    os.close(handle)
    write_wav(audio, rate, path)
    return path


def play(clips, mine):
    while True:
        path = clips.get()
        if path is None:
            return
        with lock:
            if current["id"] != mine:
                os.remove(path)
                continue
            player = subprocess.Popen([PLAYER, path])
            current["player"] = player
        player.wait()
        os.remove(path)


def write_wav(audio, rate, path):
    samples = (np.clip(audio.reshape(-1), -1.0, 1.0) * 32767).astype(np.int16)
    with wave.open(path, "wb") as out:
        out.setnchannels(1)
        out.setsampwidth(2)
        out.setframerate(rate)
        out.writeframes(samples.tobytes())


def handle(conn):
    global last_used
    with conn:
        line = conn.makefile("r").readline()
    try:
        request = json.loads(line)
    except ValueError:
        return
    last_used = time.time()
    if request.get("stop"):
        stop()
    elif request.get("say"):
        parts = request["say"]
        if isinstance(parts, str):
            parts = [{"text": parts}]
        say(parts, request.get("voice") or "af_heart", request.get("engine") or "kokoro")
    last_used = time.time()


def leave_when_idle(server):
    while True:
        time.sleep(30)
        with lock:
            playing = current["player"] is not None and current["player"].poll() is None
        if not playing and time.time() - last_used > IDLE:
            server.close()
            os.remove(SOCKET)
            os._exit(0)


def main():
    if os.path.exists(SOCKET):
        os.remove(SOCKET)
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(SOCKET)
    os.chmod(SOCKET, 0o600)
    server.listen(8)
    threading.Thread(target=leave_when_idle, args=(server,), daemon=True).start()
    while True:
        conn, _ = server.accept()
        threading.Thread(target=handle, args=(conn,), daemon=True).start()


main()
