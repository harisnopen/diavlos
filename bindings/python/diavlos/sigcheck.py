"""Check a message's Ed25519 signature yourself, in Python.

The helper already checks every signature before it hands you a message.
This is a second, independent check: the receiving agent verifies the
sender's key over the exact bytes in SPEC section 2.3, with no trust in
the helper. Needs `cryptography` (`pip install "diavlos[verify]"`).

    keys = KeyRing.load("ops", name="crew")      # member keys, from an export
    ok, micros = keys.check(msg)                  # True/False, time taken
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import time
from typing import Any, Dict, Optional, Tuple

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

__all__ = ["KeyRing", "canonical", "json_hash", "signed_bytes", "fingerprint"]

_SIGNED = ("action_hash", "agent", "class", "expires", "from", "id", "once",
           "reply_to", "room", "to", "trace", "ts", "type", "v")


def canonical(value: Any) -> bytes:
    """Canonical JSON (SPEC section 0): sorted keys, no whitespace, UTF-8."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"),
                      ensure_ascii=False).encode("utf-8")


def json_hash(value: Any) -> str:
    return "sha256:" + hashlib.sha256(canonical(value)).hexdigest()


def signed_bytes(msg: Dict[str, Any]) -> bytes:
    """The bytes the sender signed (SPEC section 2.3). The content hash is
    always computed from the content; a stored `content_hash` is only used
    for a tombstone, whose content is gone."""
    obj = {k: msg.get(k) for k in _SIGNED}
    if msg.get("tombstone"):
        obj["content_hash"] = msg.get("content_hash")
    else:
        content = {"text": msg.get("text") or "", "data": msg.get("data")}
        if msg.get("action") is not None:
            content["action"] = msg["action"]
        obj["content_hash"] = json_hash(content)
    return canonical(obj)


def fingerprint(key: str) -> str:
    """The 16-hex fingerprint `diavlos who` shows for an `ed25519:` key."""
    return hashlib.sha256(bytes.fromhex(key.split(":", 1)[1])).hexdigest()[:16]


class KeyRing:
    """Member name -> public key for one room."""

    def __init__(self, keys: Dict[str, str], room: Optional[str] = None,
                 name: str = "default", home: Optional[str] = None):
        self.room, self.name, self.home = room, name, home
        self._keys: Dict[str, Ed25519PublicKey] = {}
        self.fingerprints: Dict[str, str] = {}
        self._set(keys)

    def _set(self, keys: Dict[str, str]) -> None:
        for who, key in keys.items():
            self._keys[who] = Ed25519PublicKey.from_public_bytes(bytes.fromhex(key.split(":", 1)[1]))
            self.fingerprints[who] = fingerprint(key)

    @staticmethod
    def _export(room: str, name: str, home: Optional[str]) -> Dict[str, str]:
        exe = os.environ.get("DIAVLOS_BIN") or shutil.which("diavlos") or "diavlos"
        cmd = [exe] + (["--home", home] if home else []) + ["--as", name, "export", room]
        out = subprocess.run(cmd, capture_output=True, text=True, check=True).stdout
        head = json.loads(out.splitlines()[0])
        return {m["name"]: m["key"] for m in head["members"] if not m.get("revoked")}

    @classmethod
    def load(cls, room: str, name: str = "default", home: Optional[str] = None) -> "KeyRing":
        """Read member keys from this helper's own copy of the room."""
        return cls(cls._export(room, name, home), room=room, name=name, home=home)

    def refresh(self) -> None:
        if self.room:
            self._set(self._export(self.room, self.name, self.home))

    def check(self, msg: Dict[str, Any]) -> Tuple[bool, float]:
        """Verify one message. Returns (ok, microseconds spent verifying).
        An unknown sender triggers one refresh (someone new joined)."""
        who = msg.get("from")
        if who not in self._keys:
            self.refresh()
        key = self._keys.get(who)
        sig = msg.get("sig") or ""
        if key is None or not sig.startswith("ed25519:"):
            return False, 0.0
        t0 = time.perf_counter()
        try:
            key.verify(bytes.fromhex(sig[8:]), signed_bytes(msg))
            ok = True
        except (InvalidSignature, ValueError):
            ok = False
        return ok, (time.perf_counter() - t0) * 1e6
