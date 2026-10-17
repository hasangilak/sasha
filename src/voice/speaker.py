"""sasha's speaker: keeps a voice model loaded and says what it's sent.

sasha writes this file beside the voice models and starts it on first
use: `python speaker.py SOCKET VOICE_DIR`. Each connection sends one line
of JSON, either
  {"say": [{"text": "...", "tone": "pleased"}, ...], "voice": "bm_fable",
   "engine": "chatterbox"}
or {"stop": true}. Something new to say cuts off what it was saying.

Kokoro reads plainly in any of its voices. Chatterbox speaks with
feeling: it copies the voice, and the delivery, from a recording of that
voice in the part's mood. Kokoro reads a plain passage once; Chatterbox
then reads one passage per mood from it, each kept in VOICE_DIR/voice. Parts play one after another while the next is being made. The
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

# Each tone: how much feeling (Chatterbox's exaggeration), how deliberate
# the pace (its cfg_weight: lower is slower), and what the voice reads in
# that mood once, for Chatterbox to copy the delivery from.
TONES = {
    "calm": (0.3, 0.5, "Here is where things stand. Everything is running as expected, and nothing needs you right now."),
    "warm": (0.45, 0.5, "Good to see you. I hope your day is going well; everything here is in good hands."),
    "gentle": (0.25, 0.35, "Take your time. There's no rush at all, and I'll be right here when you're ready."),
    "serious": (0.4, 0.6, "This is important. The migration can't be undone, so we need to get this decision right."),
    "thoughtful": (0.35, 0.35, "Hmm. Let me think about that. There are two ways to go, and each has its trade-offs."),
    "tired": (0.15, 0.3, "It's been a long day. The last tests are still running, and honestly, I'm ready to wrap up."),
    "pleased": (0.6, 0.45, "Nice. The tests passed and the review came back clean. That went well."),
    "proud": (0.7, 0.4, "We did it. Weeks of work, and it all came together beautifully. I'm proud of this one."),
    "amused": (0.75, 0.35, "Ha! You won't believe it. The bug was a typo, all along. A single missing letter!"),
    "excited": (0.95, 0.3, "Yes! It works! Everything is green, and it's faster than ever! This is amazing!"),
    "surprised": (1.1, 0.3, "Wait, what? It finished already? That was so much faster than I expected!"),
    "relieved": (0.6, 0.35, "Phew. It's fixed. Staging is back up, and nothing was lost. What a relief."),
    "concerned": (0.65, 0.4, "Heads up. Something's not right. The client tests are failing, and I'm not sure why yet."),
    "sad": (0.5, 0.25, "I'm sorry. The release didn't make it today. We were so close, but it'll have to wait."),
    "frustrated": (0.85, 0.35, "Ugh. The same bug, again. Every fix breaks something else, and it's getting old."),
    "urgent": (1.0, 0.3, "Stop! This needs you right now. The cleanup is about to delete production data!"),
}
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
    """A plain recording of `voice` for Chatterbox to copy, made once by Kokoro."""
    path = os.path.join(VOICE_DIR, f"{voice}.wav")
    if not os.path.exists(path):
        kokoro = model("kokoro")
        audio = np.concatenate(
            [np.array(r.audio).reshape(-1) for r in kokoro.generate(text=REFERENCE, voice=voice, lang_code=voice[0])]
        )
        write_wav(audio, kokoro.sample_rate, path)
    return path


def mood(voice, tone):
    """A recording of `voice` in `tone`, made once by Chatterbox from the
    plain one: copying it carries the mood's delivery, not just the voice."""
    folder = os.path.join(VOICE_DIR, voice)
    path = os.path.join(folder, f"{tone}.wav")
    if not os.path.exists(path):
        os.makedirs(folder, exist_ok=True)
        feeling, pace, passage = TONES[tone]
        box = model("chatterbox")
        results = box.generate(text=passage, ref_audio=reference(voice), exaggeration=feeling, cfg_weight=pace)
        audio = np.concatenate([np.array(r.audio).reshape(-1) for r in results])
        write_wav(audio, box.sample_rate, path)
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
            tone = tone if tone in TONES else "warm"
            feeling, pace, _ = TONES[tone]
            results = box.generate(text=text, ref_audio=mood(voice, tone), exaggeration=feeling, cfg_weight=pace)
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
