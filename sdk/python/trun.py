"""Report structure to trun from Python: steps, progress, metrics, notes.

Single file, standard library only. Copy it next to your script or put it on
PYTHONPATH. Outside trun (no ``TRUN_EVENTS`` in the environment) every call is a
cheap no-op, so instrumented code runs unchanged anywhere.

    import trun

    with trun.step("train", "Train model"):
        for epoch in range(epochs):
            ...
            trun.progress("train", epoch + 1, epochs, unit="epoch")
            trun.metric(step=epoch, loss=loss, val_loss=val_loss)
    trun.note("switching to lr schedule B")

Messages go over the run's side channel (a Unix socket or Windows named pipe), not
stdout, so program output stays clean and high-frequency metrics are cheap.
See docs/04-progress-protocol.md for the message schema.
"""

from __future__ import annotations

import contextlib
import json
import math
import os
import socket
import sys
import threading
from typing import Any, Iterator, Optional

__all__ = [
    "enabled",
    "step",
    "step_begin",
    "step_end",
    "progress",
    "metric",
    "note",
    "info",
    "warn",
    "error",
    "heartbeat",
    "expect_silence",
]

_ENDPOINT = os.environ.get("TRUN_EVENTS")
_lock = threading.Lock()
_chan: Any = None
_broken = False


def enabled() -> bool:
    """True when running under trun with a side channel."""
    return bool(_ENDPOINT) and not _broken


def _open() -> Any:
    if sys.platform == "win32":
        return open(_ENDPOINT, "w", encoding="utf-8", buffering=1)  # named pipe
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(_ENDPOINT)
    return s.makefile("w", encoding="utf-8", buffering=1)


def _num(v: Any) -> Any:
    """JSON has no NaN/inf; trun accepts them as strings (and must see them)."""
    if isinstance(v, float) and not math.isfinite(v):
        return "NaN" if math.isnan(v) else ("inf" if v > 0 else "-inf")
    return v


def _send(msg: dict) -> None:
    global _chan, _broken
    if not _ENDPOINT or _broken:
        return
    line = json.dumps(msg, separators=(",", ":")) + "\n"
    with _lock:
        try:
            if _chan is None:
                _chan = _open()
            _chan.write(line)
            _chan.flush()
        except OSError as e:
            # Never let monitoring break the program being monitored.
            _broken = True
            print(f"trun: side channel unavailable ({e}); further events dropped", file=sys.stderr)


def step_begin(id: str, name: Optional[str] = None, parent: Optional[str] = None) -> None:
    _send({"kind": "step_begin", "id": id, "name": name, "parent": parent})


def step_end(id: Optional[str] = None, status: str = "ok") -> None:
    """status: ok | failed | skipped. Without id, ends the most recent open step."""
    _send({"kind": "step_end", "id": id, "status": status})


@contextlib.contextmanager
def step(id: str, name: Optional[str] = None, parent: Optional[str] = None) -> Iterator[None]:
    """A step that ends ok, or failed if the block raises."""
    step_begin(id, name, parent)
    try:
        yield
    except BaseException:
        step_end(id, "failed")
        raise
    step_end(id, "ok")


def progress(id: Optional[str], current: float, total: Optional[float] = None, unit: Optional[str] = None) -> None:
    _send({"kind": "progress", "id": id, "current": current, "total": total, "unit": unit})


def metric(step: Optional[int] = None, **values: float) -> None:
    """trun.metric(step=1200, loss=0.31, val_loss=0.4). NaN/inf are kept."""
    if not values:
        return
    _send({"kind": "metric", "values": {k: _num(float(v)) for k, v in values.items()}, "step": step})


def note(text: str) -> None:
    """Shown prominently to the human and included in agent digests."""
    _send({"kind": "note", "text": str(text)})


def info(text: str) -> None:
    _send({"kind": "log", "level": "info", "text": str(text)})


def warn(text: str) -> None:
    _send({"kind": "log", "level": "warn", "text": str(text)})


def error(text: str) -> None:
    _send({"kind": "log", "level": "error", "text": str(text)})


def heartbeat() -> None:
    """I'm alive, without printing anything."""
    _send({"kind": "heartbeat"})


def expect_silence(seconds: Optional[float]) -> None:
    """This phase is legitimately quiet for `seconds` (None = back to default)."""
    _send({"kind": "expect", "silence_ms": None if seconds is None else int(seconds * 1000)})
