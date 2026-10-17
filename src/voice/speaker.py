"""sasha's speaker: keeps a voice model loaded and says what it's sent.

sasha writes this file beside the voice models and starts it when the
tower starts, or on first use: `python speaker.py SOCKET VOICE_DIR [VOICE
ENGINE]`, which gets that voice ready straight away. Each connection sends one line
of JSON, either
  {"say": [{"text": "...", "tone": "pleased"}, ...], "voice": "bm_fable",
   "engine": "chatterbox", "pace": 0.9}
or {"stop": true}, or {"status": true}, answered with {"speaking": true or
false, "version": ...}, or {"quit": true}. Something new to say cuts off what it was saying.

Kokoro reads plainly in any of its voices. Chatterbox speaks with
feeling: it copies the voice, and the delivery, from a recording of that
voice in the part's mood. Kokoro reads a plain passage once; Chatterbox
then reads one passage per mood from it, each kept in VOICE_DIR/voice.
Speech is made a sentence at a time and plays without gaps, the first
sentence while the next is made, so it starts in about a second. All the
voice work happens on one thread, since MLX keeps a model to the thread
that loaded it; the moods are readied in idle moments. The speaker
leaves after a couple of hours with nothing to say.
"""

import json
import os
import queue
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import traceback
import wave

import numpy as np
from mlx_audio.tts.audio_player import AudioPlayer
from mlx_audio.tts.utils import load_model

SOCKET = sys.argv[1]
VOICE_DIR = sys.argv[2]
VOICE = sys.argv[3] if len(sys.argv) > 3 else None
ENGINE = sys.argv[4] if len(sys.argv) > 4 else "kokoro"
KOKORO = os.environ.get("SASHA_KOKORO_MODEL", "mlx-community/Kokoro-82M-bf16")
CHATTERBOX = os.environ.get("SASHA_CHATTERBOX_MODEL", "mlx-community/Chatterbox-TTS-fp16")
# Tests set a player that gets each piece of audio as a file.
PLAYER = os.environ.get("SASHA_PLAYER")
IDLE = 2 * 60 * 60
# Breaths: after a sentence, and between parts (at normal pace).
SENTENCE_PAUSE = 0.25
PAUSE = 0.45
# Slows speech without lowering the voice (Chatterbox has no pace of its own).
FFMPEG = shutil.which("ffmpeg") or next((p for p in ("/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg") if os.path.exists(p)), None)
# How much speech is buffered before playing starts.
BUFFER_FIRST = 0.4

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
# Chatterbox's take on each mood recording, worked out once.
conds = {}
lock = threading.Lock()
# Which request is current, and the player playing it.
current = {"id": 0, "player": None, "busy": False}
last_used = time.time()
# What there is to say, for the worker, newest last.
jobs = queue.Queue()


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


def conditionals(voice, tone):
    """What Chatterbox makes of `voice` in `tone`, kept for every later part."""
    if (voice, tone) not in conds:
        box = model("chatterbox")
        conds[(voice, tone)] = box.prepare_conditionals(mood(voice, tone), 24000, TONES[tone][0])
    return conds[(voice, tone)]


def warm_up_next():
    """Gets one more of the voice's moods ready, if any is left; false when
    all are. The model first."""
    try:
        if ENGINE == "chatterbox" and VOICE:
            for tone in TONES:
                if (VOICE, tone) not in conds:
                    conditionals(VOICE, tone)
                    return True
        # Kokoro too: it's small, a voice of its own, and the fallback.
        if "kokoro" not in models:
            model("kokoro")
            return True
    except Exception:
        traceback.print_exc()
    return False


def stop():
    with lock:
        current["id"] += 1
        player = current["player"]
        current["player"] = None
        current["busy"] = False
    if player:
        player.cut()


def still(mine):
    with lock:
        return current["id"] == mine


class Speakers:
    """Plays audio as it arrives, without gaps."""

    def __init__(self):
        self.player = None

    def add(self, audio, rate):
        if self.player is None:
            self.player = AudioPlayer(sample_rate=rate)
            self.player.min_buffer_seconds = BUFFER_FIRST
        self.player.queue_audio(audio)

    def finish(self):
        if self.player:
            self.player.wait_for_drain()
            self.player.stop_stream()

    def cut(self):
        if self.player:
            self.player.flush()


class Files:
    """Hands each piece of audio to `PLAYER` as a file, as it arrives."""

    def __init__(self):
        self.players = []

    def add(self, audio, rate):
        handle, path = tempfile.mkstemp(suffix=".wav")
        os.close(handle)
        write_wav(audio, rate, path)
        self.players.append(subprocess.Popen([PLAYER, path]))

    def finish(self):
        for player in self.players:
            player.wait()

    def cut(self):
        for player in self.players:
            if player.poll() is None:
                player.kill()


def at_pace(audio, rate, pace):
    """`audio` spoken at `pace` (0.9 is a tenth slower), pitch kept."""
    if abs(pace - 1.0) < 0.01 or not FFMPEG or not len(audio):
        return audio
    stretched = subprocess.run(
        [FFMPEG, "-loglevel", "error", "-f", "f32le", "-ar", str(rate), "-ac", "1", "-i", "pipe:0",
         "-filter:a", f"atempo={pace}", "-f", "f32le", "pipe:1"],
        input=audio.astype(np.float32).tobytes(),
        capture_output=True,
    )
    if stretched.returncode != 0 or not stretched.stdout:
        return audio
    return np.frombuffer(stretched.stdout, dtype=np.float32)


def silence(seconds, rate, pace):
    return np.zeros(int(rate * seconds / pace), dtype=np.float32)


def say(parts, voice, engine, mine, pace):
    out = Files() if PLAYER else Speakers()
    with lock:
        if current["id"] != mine:
            return
        current["player"] = out
    try:
        for part in parts:
            text = part.get("text", "").strip()
            if not text:
                continue
            rate = 24000
            for audio, rate in pieces(text, part.get("tone") or "warm", voice, engine, pace):
                if not still(mine):
                    return
                out.add(at_pace(audio, rate, pace) if engine == "chatterbox" else audio, rate)
                out.add(silence(SENTENCE_PAUSE, rate, pace), rate)
            out.add(silence(PAUSE - SENTENCE_PAUSE, rate, pace), rate)
    except Exception:
        traceback.print_exc()
    # Waiting for it to finish playing isn't the worker's job.
    def finished():
        out.finish()
        with lock:
            if current["id"] == mine:
                current["busy"] = False

    threading.Thread(target=finished, daemon=True).start()


def pieces(text, tone, voice, engine, pace):
    """`text` as audio, a piece at a time as it's made: (samples, rate)."""
    if engine == "chatterbox":
        try:
            box = model("chatterbox")
            tone = tone if tone in TONES else "warm"
            feeling, pace, _ = TONES[tone]
            # A sentence at a time: the first one starts playing while the
            # next is made, and nothing is cut mid-sentence.
            for sentence in re.split(r"(?<=[.!?])\s+", text):
                if not sentence.strip():
                    continue
                results = box.generate(
                    text=sentence,
                    conds=conditionals(voice, tone),
                    exaggeration=feeling,
                    cfg_weight=pace,
                    verbose=False,
                )
                audio = [np.array(result.audio, dtype=np.float32).reshape(-1) for result in results]
                yield np.concatenate(audio), box.sample_rate
            return
        except Exception:
            # Chatterbox can't: Kokoro reads it plainly instead.
            traceback.print_exc()
    kokoro = model("kokoro")
    # Kokoro keeps its own pace.
    for result in kokoro.generate(text=text, voice=voice, lang_code=voice[0], speed=pace):
        yield np.array(result.audio, dtype=np.float32).reshape(-1), kokoro.sample_rate


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
        # Whether it's talking, for sasha to show a stop button; and which
        # speaker this is, for sasha to replace an older one.
        if request.get("status"):
            with lock:
                speaking = current["busy"]
            answer = {"speaking": speaking, "version": os.environ.get("SASHA_SPEAKER_VERSION", "")}
            conn.sendall((json.dumps(answer) + "\n").encode())
            return
        if request.get("quit"):
            stop()
            os.remove(SOCKET)
            os._exit(0)
    last_used = time.time()
    if request.get("stop"):
        stop()
    elif request.get("say"):
        parts = request["say"]
        if isinstance(parts, str):
            parts = [{"text": parts}]
        stop()
        with lock:
            mine = current["id"]
            current["busy"] = True
        pace = min(max(float(request.get("pace") or 1.0), 0.5), 1.5)
        jobs.put((parts, request.get("voice") or "af_heart", request.get("engine") or "kokoro", mine, pace))
    last_used = time.time()


def worker():
    """Does all the voice work, on one thread: MLX keeps its models to the
    thread that loaded them. Readies the voice in its idle moments."""
    ready = False
    while True:
        try:
            job = jobs.get(timeout=0.2 if not ready else None)
        except queue.Empty:
            ready = not warm_up_next()
            continue
        parts, voice, engine, mine, pace = job
        if still(mine):
            say(parts, voice, engine, mine, pace)


def leave_when_idle(server):
    while True:
        time.sleep(30)
        if time.time() - last_used > IDLE:
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
    threading.Thread(target=worker, daemon=True).start()
    while True:
        conn, _ = server.accept()
        threading.Thread(target=handle, args=(conn,), daemon=True).start()


main()
