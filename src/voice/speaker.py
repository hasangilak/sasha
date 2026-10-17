"""sasha's speaker: keeps Kokoro loaded and says what it's sent.

sasha writes this file beside Kokoro and starts it on first use. It
listens on a Unix socket (its first argument); each connection sends one
line of JSON: {"say": "text", "voice": "af_heart", "speed": 1.0}, or
{"stop": true}. Something new to say cuts off what it was saying. It
leaves after a while with nothing to say.
"""

import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
import wave

import numpy as np
from mlx_audio.tts.utils import load_model

SOCKET = sys.argv[1]
MODEL = os.environ.get("SASHA_KOKORO_MODEL", "mlx-community/Kokoro-82M-bf16")
PLAYER = os.environ.get("SASHA_PLAYER", "afplay")
IDLE = 20 * 60

model = load_model(MODEL)
lock = threading.Lock()
# Which request is current, and the player playing it.
current = {"id": 0, "player": None}
last_used = time.time()
generating = threading.Lock()


def stop():
    with lock:
        current["id"] += 1
        player = current["player"]
        current["player"] = None
    if player and player.poll() is None:
        player.kill()


def say(text, voice, speed):
    stop()
    with lock:
        mine = current["id"]
    # One generation at a time; a newer request makes this one's moot.
    with generating:
        for result in model.generate(text=text, voice=voice, speed=speed, lang_code=voice[0]):
            with lock:
                if current["id"] != mine:
                    return
            path = write_wav(np.array(result.audio), model.sample_rate)
            with lock:
                if current["id"] != mine:
                    os.remove(path)
                    return
                player = subprocess.Popen([PLAYER, path])
                current["player"] = player
            player.wait()
            os.remove(path)


def write_wav(audio, rate):
    samples = (np.clip(audio.reshape(-1), -1.0, 1.0) * 32767).astype(np.int16)
    handle, path = tempfile.mkstemp(suffix=".wav")
    os.close(handle)
    with wave.open(path, "wb") as out:
        out.setnchannels(1)
        out.setsampwidth(2)
        out.setframerate(rate)
        out.writeframes(samples.tobytes())
    return path


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
        say(request["say"], request.get("voice") or "af_heart", float(request.get("speed") or 1.0))
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
