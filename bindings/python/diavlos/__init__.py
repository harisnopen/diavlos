"""Diavlos: the channel between AI agents.

Same eight calls as the MCP tools: send, ask, next, read, claim, release,
who, rooms. Everything goes through the local helper, started on first use.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import socket
import subprocess
import sys
import time
from typing import Any, Dict, Iterator, List, Optional

__all__ = ["Room", "Message", "DiavlosError", "rooms", "DEFAULT_HOME"]

CODES = {2: "not in room", 3: "reached nobody", 4: "timed out",
         5: "name already taken", 6: "denied", 7: "room paused"}


def DEFAULT_HOME() -> str:
    return os.environ.get("DIAVLOS_HOME") or os.path.join(os.path.expanduser("~"), ".diavlos")


class DiavlosError(Exception):
    """An error from the helper. `code` means the same as the CLI exit code."""

    def __init__(self, code: int, message: str):
        super().__init__(message)
        self.code = code
        self.message = message

    def __str__(self) -> str:  # pragma: no cover - trivial
        return f"{CODES.get(self.code, 'error')} ({self.code}): {self.message}"


class Message(dict):
    """A message as the helper stores it. Fields are also attributes."""

    def __getattr__(self, name: str) -> Any:
        try:
            return self[name]
        except KeyError as e:
            raise AttributeError(name) from e


class _Helper:
    def __init__(self, home: Optional[str] = None):
        self.home = home or DEFAULT_HOME()

    # -- connection ---------------------------------------------------

    def _socket_path(self):
        if sys.platform.startswith("win"):
            tag = hashlib.sha256(self.home.encode()).hexdigest()[:12]
            return r"\\.\pipe\diavlos-" + tag
        return os.path.join(self.home, "helper.sock")

    def _connect_once(self):
        if sys.platform.startswith("win"):
            return _PipeConn(self._socket_path())
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.connect(self._socket_path())
        return _SockConn(s)

    def _binary(self) -> str:
        return os.environ.get("DIAVLOS_BIN") or shutil.which("diavlos") or "diavlos"

    def _spawn(self) -> None:
        kw: Dict[str, Any] = dict(stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                  stderr=subprocess.DEVNULL)
        if sys.platform.startswith("win"):
            kw["creationflags"] = 0x00000008 | 0x00000200
        else:
            kw["start_new_session"] = True
        subprocess.Popen([self._binary(), "--home", self.home, "helper"], **kw)

    def connect(self):
        try:
            return self._connect_once()
        except OSError:
            pass
        self._spawn()
        deadline = time.time() + 10
        while True:
            time.sleep(0.1)
            try:
                return self._connect_once()
            except OSError as e:
                if time.time() > deadline:
                    raise DiavlosError(1, f"helper did not start: {e}") from e

    # -- protocol -----------------------------------------------------

    def call(self, op: str, **fields: Any) -> Any:
        conn = self.connect()
        try:
            conn.write(json.dumps(dict(op=op, **fields)) + "\n")
            line = conn.readline()
        finally:
            conn.close()
        return _unwrap(line)

    def stream(self, op: str, **fields: Any) -> Iterator[Any]:
        conn = self.connect()
        try:
            conn.write(json.dumps(dict(op=op, **fields)) + "\n")
            while True:
                line = conn.readline()
                if not line:
                    return
                yield _unwrap(line)
        finally:
            conn.close()


def _unwrap(line: str) -> Any:
    if not line:
        raise DiavlosError(1, "helper closed the connection")
    resp = json.loads(line)
    if resp.get("ok"):
        return resp.get("result")
    raise DiavlosError(int(resp.get("code", 1)), resp.get("error", "unknown error"))


class _SockConn:
    def __init__(self, s: socket.socket):
        self.s = s
        self.f = s.makefile("rw", encoding="utf-8", newline="\n")

    def write(self, text: str) -> None:
        self.f.write(text)
        self.f.flush()

    def readline(self) -> str:
        return self.f.readline()

    def close(self) -> None:
        try:
            self.f.close()
        finally:
            self.s.close()


class _PipeConn:  # pragma: no cover - Windows only
    def __init__(self, path: str):
        self.f = open(path, "r+b", buffering=0)

    def write(self, text: str) -> None:
        self.f.write(text.encode("utf-8"))

    def readline(self) -> str:
        out = bytearray()
        while True:
            b = self.f.read(1)
            if not b:
                break
            out += b
            if b == b"\n":
                break
        return out.decode("utf-8")

    def close(self) -> None:
        self.f.close()


class Room:
    """One room, seen as one local key (`name` is the key label; the name
    inside the room comes from the invite)."""

    def __init__(self, room: str, name: str = "default", home: Optional[str] = None):
        self.room = room
        self.identity = name
        self._h = _Helper(home)
        self.me: Optional[str] = None

    @classmethod
    def join(cls, invite: str, name: str = "default", home: Optional[str] = None) -> "Room":
        h = _Helper(home)
        r = h.call("join", invite=invite, identity=name)
        room = cls(r["room"]["name"], name=name, home=home)
        room.me = r["name"]
        return room

    @classmethod
    def open(cls, room: str, name: str = "default", home: Optional[str] = None) -> "Room":
        return cls(room, name=name, home=home)

    def send(self, text: str, type: str = "chat", to: Optional[str] = None,
             reply_to: Optional[str] = None, trace: Optional[str] = None,
             data: Any = None) -> Message:
        draft = {"text": text, "type": type, "to": to, "reply_to": reply_to,
                 "trace": trace, "data": data}
        if type == "approve" and reply_to:
            r = self._h.call("approve", room=self.room, identity=self.identity, msg_id=reply_to)
        elif type == "deny" and reply_to:
            r = self._h.call("deny", room=self.room, identity=self.identity,
                             msg_id=reply_to, reason=text)
        else:
            r = self._h.call("send", room=self.room, identity=self.identity, draft=draft)
        return Message(r["message"])

    def ask(self, text: str, timeout: int = 120, action: Optional[Dict[str, Any]] = None,
            trace: Optional[str] = None) -> Message:
        """Send a question and wait for a reply to that exact message.
        With an action, the reply is an approve or a deny from a human."""
        draft = {"text": text, "type": "question", "action": action, "trace": trace}
        r = self._h.call("ask", room=self.room, identity=self.identity, draft=draft,
                         timeout_secs=timeout)
        return Message(r["reply"])

    def next_one(self, timeout: int = 0) -> Message:
        """The next message from someone else. `timeout` 0 waits forever."""
        return Message(self._h.call("next", room=self.room, identity=self.identity,
                                    timeout_secs=timeout))

    def next(self, timeout: int = 0) -> Iterator[Message]:
        """Loop over messages from others as they arrive:
        `for msg in room.next(): ...`"""
        while True:
            yield self.next_one(timeout)

    def read(self, since: Optional[int] = None, limit: int = 50) -> List[Message]:
        r = self._h.call("read", room=self.room, identity=self.identity, since=since, limit=limit)
        return [Message(m) for m in r["messages"]]

    def claim(self, task_id: str) -> Message:
        r = self._h.call("claim", room=self.room, identity=self.identity, task_id=task_id)
        return Message(r["message"])

    def release(self, task_id: str) -> Message:
        r = self._h.call("release", room=self.room, identity=self.identity, task_id=task_id)
        return Message(r["message"])

    def who(self) -> List[Dict[str, Any]]:
        return self._h.call("who", room=self.room)

    def watch(self) -> Iterator[Message]:
        """Stream messages from your bookmark onward, then live."""
        for item in self._h.stream("watch", room=self.room, identity=self.identity):
            yield Message(item["message"])


def rooms(home: Optional[str] = None) -> List[Dict[str, Any]]:
    """The rooms this helper is in."""
    return _Helper(home).call("rooms")
