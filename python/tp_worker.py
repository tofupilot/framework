#!/usr/bin/env python3
"""
NDJSON TCP-based Python worker with complete standalone implementation.
No external dependencies beyond the Python standard library.
"""


import sys
import json
import logging
import difflib
import math
import os
import re
import time
import traceback
import importlib.util
import warnings
import base64
import inspect
import queue
import threading
import random
import reprlib
import socket
from pathlib import Path
from typing import Dict, Any, Optional, List, Union, Literal

# Save original stderr before any redirection
_original_stderr = sys.__stderr__


# ============================================================================
# Failure context, `tofupilot run --locals`: the variables of the phase's
# own frame where it raised, for the person or agent reading the failure in
# the terminal or on `--json`. Local only: the engine never puts them in the
# run record, nor on the station wire.
# Asked per job by the engine (`capture_locals` on the command, from
# `tofupilot run --locals`), so nothing in the environment can turn it on.
_LOCALS_MAX = 30
# Long enough that the CLI, which knows the declared secret values, masks
# before it shortens. A secret straddling this cut would lose its tail and
# escape that mask: the cut is a bound on the payload, not a guarantee.
_LOCALS_VALUE_CHARS = 4096
_LOCALS_TOTAL_CHARS = 32768
# A name is a secret when one of its words always is (`db_password`), or
# when its LAST word is one (`auth_token`, but not `token_count`).
# `passed`, `bypass` or `author` contain no such word.
# `session`, `auth` and `pwd` are not in the lists: on a bench they name
# an instrument resource, a flag and a directory as often as a secret.
_SECRET_ALWAYS = {
    "password", "passwd", "passphrase", "secret", "apikey", "privatekey",
    "credential", "credentials", "jwt", "cookie",
}
_SECRET_LAST = _SECRET_ALWAYS | {"token", "authorization"}
# Secrets inside a value whose own name looks harmless: a password in a
# URL (`scheme://user:pass@host`), a bearer token, and any value under a
# secret-looking name, as a dict entry (`{'password': ...}`), a keyword in
# an object's repr (`Cfg(password='...')`, a dataclass, a namedtuple) or a
# `key=value` pair (`Server=db;Password=...;`). Shaped narrowly: a sentence
# that says "token" or "Basic" stays readable.
_URL_PASSWORD = re.compile(r"(\b[a-zA-Z][a-zA-Z0-9+.-]*://[^:/@\s'\"]+:)([^/\s'\"]+)(@)")
_BEARER = re.compile(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]{8,}")
# A JSON Web Token, whatever it is named.
_JWT = re.compile(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]+")
_SECRET_HOLDERS = (str, bytes, bytearray, int, float, tuple, list, set, frozenset, dict)
# Where a secret value starts: after a quoted dict key and its colon, or
# after `name=` (a keyword in a repr, a `key=value` pair). The value itself
# is consumed by `_value_end`, which follows nesting a regex cannot.
# Any key a config dict uses (`db.password`, `mqtt/password`), bounded so
# the scan stays linear.
_SECRET_ENTRY = re.compile(r"""(['"]([^'"\\\n]{1,64})['"]\s*:\s*)""")
_SECRET_ASSIGN = re.compile(r"""(\b([A-Za-z_][A-Za-z0-9_ ]{0,64}?)\s*=(?!=)\s*)""")
_CLOSE = {"(": ")", "[": "]", "{": "}"}


def _value_end(text, i):
    """End of the value starting at `text[i]`: a string or bytes literal, a
    bracketed value or a call (`SecretStr('...')`) to its matching close
    whatever the nesting, or a bare token up to the next separator."""
    n = len(text)
    depth = []
    quote = None
    j = i
    if j < n and text[j] in "bB" and j + 1 < n and text[j + 1] in "'\"":
        j += 1
    while j < n:
        c = text[j]
        if quote:
            if c == "\\":
                j += 2
                continue
            if c == quote:
                quote = None
                if not depth:
                    return j + 1
        elif c in "'\"":
            quote = c
        elif c in _CLOSE:
            depth.append(_CLOSE[c])
        elif depth and c == depth[-1]:
            depth.pop()
            if not depth:
                return j + 1
        elif not depth and (c in ",;&}])" or c.isspace()):
            return j
        j += 1
    return n


def _looks_secret(name):
    words = [w for w in re.split(r"[^a-z0-9]+", name.lower()) if w]
    if not words:
        return False
    joined = [a + b for a, b in zip(words, words[1:])]
    if any(w in _SECRET_ALWAYS for w in words + joined):
        return True
    return words[-1] in _SECRET_LAST or (len(words) > 1 and joined[-1] in _SECRET_LAST)


def _mask_value_text(text):
    text = _URL_PASSWORD.sub(r"\1***\3", text)
    text = _BEARER.sub(r"\1***", text)
    text = _JWT.sub("***", text)
    for pattern in (_SECRET_ENTRY, _SECRET_ASSIGN):
        out = []
        pos = 0
        for m in pattern.finditer(text):
            if m.start() < pos:
                continue
            # Judged on the last two words of the name: the name group of a
            # `key=value` in free text runs back over the words before it
            # ("password reset requested for user=bob"). Before the value is
            # scanned, so `a=a=a=…` stays linear.
            if not _looks_secret(" ".join(m.group(2).split()[-2:])):
                continue
            start = m.end()
            end = _value_end(text, start)
            if end == start or text[start:end] in ("None", "True", "False"):
                continue
            out.append(text[pos:start])
            out.append("***")
            pos = end
        out.append(text[pos:])
        text = "".join(out)
    return text


_LOCALS_REPR = reprlib.Repr()
_LOCALS_REPR.maxlist = _LOCALS_REPR.maxtuple = _LOCALS_REPR.maxset = 50
_LOCALS_REPR.maxdict = 50
_LOCALS_REPR.maxstring = _LOCALS_REPR.maxother = _LOCALS_VALUE_CHARS
_LOCALS_REPR.maxlevel = 4


def _failure_locals(exc):
    """`[{"name", "value"}]` of the innermost frame of the procedure's own
    code in `exc`'s traceback (not this worker, not an installed package).
    Values are `repr`s, truncated; a name that looks like a secret is
    masked. Never raises: a failure context must not hide the failure."""
    try:
        cwd = os.path.realpath(os.getcwd()) + os.sep
        frame = None
        tb = exc.__traceback__
        while tb is not None:
            filename = os.path.realpath(tb.tb_frame.f_code.co_filename)
            if (
                filename.startswith(cwd)
                and os.sep + "site-packages" + os.sep not in filename
                and os.path.basename(filename) not in ("tp_worker.py", "tp_plug.py")
            ):
                frame = tb.tb_frame
            tb = tb.tb_next
        if frame is None:
            return []
        out = []
        total = 0
        for name, value in list(frame.f_locals.items()):
            if name.startswith("__") or inspect.ismodule(value) or inspect.isfunction(value) or inspect.isclass(value):
                continue
            # Masked whole when it can hold the secret itself (text, a
            # number, a `(user, password)` tuple, a dict): `None`, a bool
            # and any other object show, their own secrets masked inside
            # them below.
            if (
                _looks_secret(name)
                and isinstance(value, _SECRET_HOLDERS)
                and not isinstance(value, bool)
            ):
                text = "***"
            else:
                try:
                    if isinstance(value, str):
                        # Masked as written, then quoted: `repr` would escape
                        # the quotes of a dict printed into a message
                        # (`f"failed for {cfg}"`) and hide its keys.
                        text = repr(_mask_value_text(value[:_LOCALS_VALUE_CHARS]))
                    else:
                        # Bounded: a million-sample list is not formatted whole.
                        text = _mask_value_text(_LOCALS_REPR.repr(value))
                except Exception as e:  # a broken __repr__
                    text = f"<unrepresentable: {type(e).__name__}>"
                if len(text) > _LOCALS_VALUE_CHARS:
                    text = text[:_LOCALS_VALUE_CHARS]
            total += len(text)
            if total > _LOCALS_TOTAL_CHARS:
                break
            out.append({"name": name, "value": text})
            if len(out) >= _LOCALS_MAX:
                break
        return out
    except Exception:
        return []


# Debug mode (debugpy)
# ============================================================================
# Enabled by TP_DEBUG=1 (set per-spawn on pool workers by the debug run path).
# Contract documented in private/design/debugpy-support.md.
_debugpy_started = False
_debugpy_attempted = False


def _debug_enabled() -> bool:
    # Strict "1" match: a stray TP_DEBUG=0 / TP_DEBUG=false in the
    # environment must NOT enable debug mode.
    return os.environ.get("TP_DEBUG") == "1"


def _maybe_start_debugpy():
    """Start the debugpy DAP listener once, before the phase-serving loop.

    Idempotent: safe to call more than once per process. Must be called
    AFTER the transport port handshake so the spawning side isn't blocked
    waiting for a port line while we block waiting for the IDE.
    """
    global _debugpy_started, _debugpy_attempted
    if not _debug_enabled() or _debugpy_attempted:
        return
    # Guard re-entry separately from success: _debugpy_started must flip
    # only after a debugger is actually listening, because the phase
    # timeout is disabled off _debugpy_started. If the import or listen
    # fails, _debugpy_started stays False so the timeout stays enforced.
    _debugpy_attempted = True

    port = 5678
    try:
        port = int(os.environ.get("TP_DEBUG_PORT", "5678"))
    except ValueError:
        pass

    try:
        import debugpy
    except ImportError:
        print(
            "TP_DEBUG set but debugpy is not installed - run: uv add debugpy",
            file=_original_stderr,
            flush=True,
        )
        return

    try:
        debugpy.listen(("localhost", port))
    except RuntimeError:
        # A phase module already called debugpy.listen() itself.
        print(
            "debugpy is already listening - remove your manual "
            "debugpy.listen() call or unset TP_DEBUG",
            file=_original_stderr,
            flush=True,
        )
        return
    except OSError as e:
        # Port already bound by another process, etc.
        print(
            f"debugpy could not listen on localhost:{port}: {e}",
            file=_original_stderr,
            flush=True,
        )
        return

    print(f"waiting for debugger on localhost:{port} ...", file=_original_stderr, flush=True)
    debugpy.wait_for_client()
    _debugpy_started = True


def _debug_this_thread():
    """Enable debugpy tracing on the current (phase) thread. No-op unless
    debug mode is on and debugpy is importable/listening."""
    if not _debug_enabled() or not _debugpy_started:
        return
    try:
        import debugpy

        debugpy.debug_this_thread()
    except Exception:
        pass


# ============================================================================
# Dashboard access (the `dashboard` parameter of a phase or plug)
# ============================================================================


class DashboardUnavailable(RuntimeError):
    """Raised when a phase or plug asks for `dashboard` and this run has no
    identity to act as. The message names the fix."""


_dashboard_lock = threading.Lock()
_dashboard_client = None


def _dashboard_ssl_context(ca_cert):
    """The default trust (certifi, which httpx would use anyway) plus the
    instance's private CA when the CLI passed one. Added, not replaced: the
    procedure's other HTTPS calls keep working."""
    import ssl

    try:
        import certifi

        ctx = ssl.create_default_context(cafile=certifi.where())
    except Exception:
        ctx = ssl.create_default_context()
    if ca_cert:
        ctx.load_verify_locations(cafile=ca_cert)
    return ctx


def dashboard_client():
    """The TofuPilot API client a phase or plug receives as `dashboard`.

    Built once per process from the environment the CLI set for a run
    that uploads: TOFUPILOT_API_KEY, TOFUPILOT_URL
    (the instance base, no /api) and TOFUPILOT_CA_CERT. The identity is the
    one the run uploads with; what it may call is what that identity may
    call on the server."""
    global _dashboard_client
    with _dashboard_lock:
        if _dashboard_client is not None:
            return _dashboard_client
        api_key = (os.environ.get("TOFUPILOT_API_KEY") or "").strip()
        base_url = (os.environ.get("TOFUPILOT_URL") or "").strip()
        if not api_key or not base_url:
            raise DashboardUnavailable(
                "The dashboard is not available to this run: run with --upload or on a "
                "station. The procedure acts as the identity the run uploads with, and a "
                "run that does not upload has none."
            )
        try:
            import httpx
            from tofupilot.v2 import TofuPilot
        except ImportError as e:
            raise DashboardUnavailable(
                "The `tofupilot` package is not installed in the procedure's environment: "
                "add `tofupilot` to the dependencies in pyproject.toml"
            ) from e
        http = httpx.Client(
            verify=_dashboard_ssl_context(os.environ.get("TOFUPILOT_CA_CERT") or None),
            # Ignored by the server today; what an audit column would read.
            headers={"x-tofupilot-origin": "procedure"},
        )
        _dashboard_client = TofuPilot(
            api_key=api_key,
            server_url=base_url.rstrip("/") + "/api",
            client=http,
            timeout_ms=15000,
        )
        return _dashboard_client


# ============================================================================
# Logging (inlined from tp_logs.py)
# ============================================================================

def _relativize_log_path(file_path):
    """Strip the worker's base directory prefix so log origins render as
    project-relative paths in the console."""
    import os as _os

    try:
        if file_path and file_path.startswith("/"):
            base_dir = _os.path.dirname(_os.path.dirname(_os.path.abspath(__file__)))
            if file_path.startswith(base_dir):
                return _os.path.relpath(file_path, base_dir)
    except Exception:
        pass
    return file_path


class Logs:
    def __repr__(self):
        return f"Logs({len(self.entries)} lines)"

    def __init__(self, job_id=None, event_queue=None):
        self.entries = []
        self.job_id = job_id
        # Same rationale as LogCapturingStream: push each line onto the live
        # event queue so the orchestrator can surface them in real time
        # (essential for debugging a phase that's force-killed before it
        # reaches its natural completion).
        self.event_queue = event_queue

    def _add_log(self, level: str, message: str, file=None, line=None):
        from datetime import datetime, timezone
        import inspect as _inspect

        file_path = file
        line_number = line
        if file_path is None and line_number is None:
            # Called through info()/warning()/... — walk two frames up to
            # find the phase code that made the call.
            frame = _inspect.currentframe()
            caller_frame = (
                frame.f_back.f_back
                if frame and frame.f_back and frame.f_back.f_back
                else None
            )
            # A line this runtime writes itself has no source to show.
            if caller_frame and caller_frame.f_code.co_filename != __file__:
                file_path = _relativize_log_path(caller_frame.f_code.co_filename)
                line_number = caller_frame.f_lineno

        timestamp = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")

        entry = {
            "timestamp": timestamp,
            "level": level,
            "message": message,
        }

        if file_path:
            entry["file"] = file_path
        if line_number:
            entry["line"] = line_number

        self.entries.append(entry)

        if self.event_queue is not None:
            try:
                live_event = {
                    "type": "PhaseLogLine",
                    "job_id": self.job_id,
                    "level": level,
                    "message": message,
                    "timestamp": timestamp,
                }
                if file_path:
                    live_event["file"] = file_path
                if line_number:
                    live_event["line"] = line_number
                self.event_queue.put(live_event)
            except Exception:
                pass

    # `log.info("powered %s", x)`, as `logging` takes it. Formatted in
    # each method: `_add_log` finds the caller two frames up.
    def info(self, message: str, *args):
        self._add_log("INFO", _log_text(message, args))

    def warning(self, message: str, *args):
        self._add_log("WARNING", _log_text(message, args))

    def error(self, message: str, *args):
        self._add_log("ERROR", _log_text(message, args))

    def debug(self, message: str, *args):
        self._add_log("DEBUG", _log_text(message, args))


def _log_text(message, args) -> str:
    if not args:
        return message if isinstance(message, str) else str(message)
    try:
        return str(message) % args
    except (TypeError, ValueError):
        # A message with no `%s` for its args: written as logging would
        # fail to, the values kept rather than the phase failed.
        return " ".join(str(a) for a in (message, *args))


class LogCapturingStream:
    encoding = "utf-8"
    errors = "replace"

    def __init__(self, logs_obj, level="INFO", job_id=None, emit_to_stderr=False,
                 event_queue=None):
        self.logs = logs_obj
        self.level = level
        self.job_id = job_id
        self.buffer = ""
        self.emit_to_stderr = emit_to_stderr
        # When provided, every captured log line is also pushed onto the
        # event queue so the Rust orchestrator sees it live — not only in
        # the final JobComplete bundle. Without this, a phase that's force-
        # killed mid-run has all its logs discarded.
        self.event_queue = event_queue
        # The phase and its own threads may print at the same time.
        # Reentrant: a `__del__` that prints, run by the GC in the middle
        # of a write on the same thread, would otherwise deadlock.
        self._lock = threading.RLock()

    def write(self, text):
        with self._lock:
            self._write_locked(text)
        return len(text)

    def _write_locked(self, text):
        self.buffer += text
        while "\n" in self.buffer:
            line, self.buffer = self.buffer.split("\n", 1)
            if line.strip():
                from datetime import datetime, timezone

                timestamp = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")

                entry = {
                    "timestamp": timestamp,
                    "level": self.level,
                    "message": line.strip(),
                }
                self.logs.entries.append(entry)

                if self.event_queue is not None:
                    try:
                        self.event_queue.put({
                            "type": "PhaseLogLine",
                            "job_id": self.job_id,
                            "level": self.level,
                            "message": line.strip(),
                            "timestamp": timestamp,
                        })
                    except Exception:
                        pass

                if self.emit_to_stderr and _original_stderr:
                    try:
                        _original_stderr.write(json.dumps(entry) + "\n")
                        _original_stderr.flush()
                    except Exception:
                        pass

    def flush(self):
        pass

    def isatty(self):
        return False

    def writable(self):
        return True

    def fileno(self):
        # Code that hands the stream to C or a subprocess needs a real
        # descriptor; the raw stderr pipe is the one the engine reads.
        return _original_stderr.fileno()


class StdlibLoggingBridge(logging.Handler):
    """Forwards stdlib `logging` records into the phase's Logs pipeline.

    Without this, a phase using `logging.getLogger(...)` gets its records
    written to stderr by Python's last-resort handler, where the stderr
    capture flattens every level to WARNING and drops DEBUG/INFO entirely.
    Attaching a real handler preserves the record's level, file and line —
    and silences the last-resort stderr output, so lines aren't doubled.
    """

    def __init__(self, logs_obj):
        super().__init__()
        self.logs = logs_obj

    def emit(self, record):
        try:
            self.logs._add_log(
                record.levelname,
                self.format(record),
                file=_relativize_log_path(record.pathname),
                line=record.lineno,
            )
        except Exception:
            pass


# ============================================================================
# Domain classes
# ============================================================================

class EventQueue:
    def __init__(self):
        self._queue = queue.Queue()
        self._closed = False
        self._lock = threading.Lock()

    def put(self, item: Any) -> None:
        with self._lock:
            if self._closed:
                return
        self._queue.put(item)

    def get(self, timeout: float = 0.1) -> Optional[Any]:
        try:
            return self._queue.get(timeout=timeout)
        except queue.Empty:
            return None

    def close(self) -> None:
        with self._lock:
            self._closed = True

    def is_closed(self) -> bool:
        with self._lock:
            return self._closed

    def empty(self) -> bool:
        return self._queue.empty()


# How each run-time settable property is checked before it leaves Python
# (`UIContext.set`). WHICH properties a component accepts is not decided
# here: the engine sends that per component in the job command, derived
# from the one allow-list in the station-protocol crate. A Rust test holds
# this dict's keys against that list, so neither side can grow a property
# the other does not know.
def _shape_string(name, value):
    if value is None or isinstance(value, str):
        return value
    raise TypeError(f"ui.set: '{name}' must be a string or None, got {type(value).__name__}")


def _shape_number(name, value):
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise TypeError(f"ui.set: '{name}' must be a number or None, got {type(value).__name__}")
    if value != value or value in (float("inf"), float("-inf")):
        raise ValueError(f"ui.set: '{name}' must be a finite number")
    return value


def _shape_length(name, value):
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise TypeError(f"ui.set: '{name}' must be a non-negative integer or None")
    return value


# The YAML loader's caps on an option list, repeated here so `ui.set`
# refuses at the call what the engine would refuse with only a log line.
# Pinned to the station-protocol constants by a Rust test
# (`python_option_caps_match_the_protocol`).
_MAX_OPTION_TEXT_CHARS = 200
_MAX_OPTIONS_WITH_IMAGES = 12


def _shape_options(name, value):
    """A list of options, each a plain string (value and label alike) or a
    dict with `value` and optional `label` and `image`. Normalised to the
    dict form the YAML uses, so every surface renders it the same way."""
    if value is None:
        return None
    if isinstance(value, (str, bytes, dict)) or not hasattr(value, "__iter__"):
        raise TypeError(
            "ui.set: 'options' must be a list of strings or of {'value', 'label'} dicts"
        )
    out = []
    for i, item in enumerate(value, start=1):
        # Trimmed like the YAML loader trims its option values: the
        # validator trims the operator's answer before comparing, so a
        # space-padded value (a CHAR column from an MES) could never match.
        if isinstance(item, str):
            option = {"value": item.strip(), "label": item.strip()}
        elif isinstance(item, dict):
            unknown = set(item) - {"value", "label", "image"}
            if unknown:
                raise ValueError(
                    f"ui.set: option {i} has unknown keys {sorted(unknown)}; "
                    "allowed: value, label, image"
                )
            if "value" not in item or not isinstance(item["value"], str):
                raise ValueError(f"ui.set: option {i} needs a string 'value'")
            option = {"value": item["value"].strip(), "label": item.get("label", item["value"])}
            if not isinstance(option["label"], str):
                raise ValueError(f"ui.set: option {i} 'label' must be a string")
            option["label"] = option["label"].strip()
            if item.get("image") is not None:
                if not isinstance(item["image"], str):
                    raise ValueError(f"ui.set: option {i} 'image' must be a string")
                option["image"] = item["image"]
        else:
            raise TypeError(
                f"ui.set: option {i} must be a string or a dict, got {type(item).__name__}"
            )
        if not option["value"].strip():
            raise ValueError(f"ui.set: option {i} has an empty value")
        for what in ("value", "label"):
            if len(option[what]) > _MAX_OPTION_TEXT_CHARS:
                raise ValueError(
                    f"ui.set: option {i} {what} is longer than {_MAX_OPTION_TEXT_CHARS} characters"
                )
        out.append(option)
    if any("image" in o for o in out) and len(out) > _MAX_OPTIONS_WITH_IMAGES:
        raise ValueError(
            f"ui.set: {len(out)} options with images (max {_MAX_OPTIONS_WITH_IMAGES} "
            "when options have images)"
        )
    values = [o["value"] for o in out]
    if len(set(values)) != len(values):
        dup = next(v for v in values if values.count(v) > 1)
        raise ValueError(f"ui.set: option value '{dup}' appears twice; values must be unique")
    return out


_SETTABLE_SHAPES = {
    "label": _shape_string,
    "description": _shape_string,
    "placeholder": _shape_string,
    "prefix": _shape_string,
    "suffix": _shape_string,
    "options": _shape_options,
    "min": _shape_number,
    "max": _shape_number,
    "step": _shape_number,
    "min_length": _shape_length,
    "max_length": _shape_length,
    "pattern": _shape_string,
    "pattern_message": _shape_string,
}


class UiWaitCancelled(Exception):
    """`ui.wait()` can never be answered: the run or the slot was stopped,
    the phase timed out, or the screen was closed. Raised from the
    `wait()` call; let it propagate unless the phase has cleanup to do."""


class _EngineChannel:
    """Engine -> Python messages of one job connection, delivered to the
    `ui.wait()` they answer. Fed by `_read_engine_messages` on a thread of
    its own; once the connection is gone every wait, current or later,
    raises `UiWaitCancelled` instead of blocking forever."""

    def __init__(self):
        self._lock = threading.Lock()
        self._waiting: Dict[str, "queue.Queue"] = {}
        self._closed: Optional[str] = None
        self._wait_count = 0
        # The last wait whose answer a phase thread actually received.
        # Sent with JobComplete so `bind:` never records an answer the
        # engine wrote after the phase had already ended.
        self._answered: Optional[str] = None
        # Panel button presses not yet read by `ui.pressed()`, by name.
        self._presses: Dict[str, int] = {}

    def open(self) -> "tuple[str, queue.Queue]":
        """A fresh wait id and the slot its reply lands in. Allocated
        under the lock: two threads of one phase waiting at once must
        not share an id, or one of them never gets its reply."""
        with self._lock:
            if self._closed is not None:
                raise UiWaitCancelled(self._closed)
            self._wait_count += 1
            wait_id = str(self._wait_count)
            slot = queue.Queue(maxsize=1)
            self._waiting[wait_id] = slot
            return wait_id, slot

    def mark_answered(self, wait_id: str) -> None:
        with self._lock:
            if self._closed is None:
                self._answered = wait_id

    def answered_wait_id(self) -> Optional[str]:
        with self._lock:
            return self._answered

    def take_presses(self, name: str) -> int:
        with self._lock:
            return self._presses.pop(name, 0)

    def deliver(self, message: Dict[str, Any]) -> None:
        if message.get("type") == "UiEvent":
            with self._lock:
                name = str(message.get("name"))
                self._presses[name] = self._presses.get(name, 0) + 1
            return
        with self._lock:
            slot = self._waiting.pop(str(message.get("wait_id")), None)
        if slot is not None:
            slot.put(message)

    def close(self, reason: str) -> None:
        with self._lock:
            if self._closed is None:
                self._closed = reason
            waiting = list(self._waiting.values())
            self._waiting.clear()
        for slot in waiting:
            slot.put({"type": "UiCancel", "reason": reason})


def _read_engine_messages(conn: socket.socket, buf: bytes, channel: _EngineChannel) -> None:
    """Read the NDJSON lines the engine writes after the job command
    (`EngineMessage` in protocol.rs) until the connection closes. `buf`
    holds whatever arrived with the command line."""
    try:
        while True:
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                if not line.strip():
                    continue
                try:
                    message = json.loads(line.decode("utf-8"))
                except ValueError:
                    print(f"Unreadable engine message: {line[:200]!r}", file=_original_stderr)
                    continue
                channel.deliver(message)
            chunk = conn.recv(65536)
            if not chunk:
                break
            buf += chunk
    except OSError:
        pass
    finally:
        channel.close(
            "The phase ended (timeout, error or stop) before the operator answered"
        )


class UIContext:
    """The operator screen of this phase, as seen from Python.

    Two writes, both one-way (Python -> engine -> every operator surface),
    and two reads:

        ui.<key> = value            set a component's value; with a panel,
                                    a key no component declares is a value
                                    the panel shows (never recorded, never
                                    in an answer)
        ui.set(key, **properties)   change a component's properties: its
                                    options, label, placeholder, bounds...
                                    (never its key or its type)
        answer = ui.wait()          block until the operator submits the
                                    screen, then get the answer
        n = ui.pressed("tare")      presses of the panel's
                                    <button name="tare"> since the last call

    Without `wait()` the operator's answer reaches the NEXT phase as a
    measurement of this one (`bind:`), and the screen waits for its
    Continue after the phase function returns.

    `set` refuses, with a Python exception at the call, what the engine
    would refuse anyway: an unknown key, a property the component type
    does not read, a malformed option, an option list past the YAML caps.
    Rules that need the whole component (min above max, a pattern the
    station cannot compile) are checked by the engine on the merged
    component and logged as an ERROR phase log when they fail; the screen
    is then left unchanged.
    """

    # `except ui.WaitCancelled:` without an import.
    WaitCancelled = UiWaitCancelled

    def __repr__(self):
        return "UIContext()"

    def __init__(
        self,
        job_id: str,
        event_queue: EventQueue,
        components=None,
        engine=None,
        panel: bool = False,
        events=None,
    ):
        object.__setattr__(self, "job_id", job_id)
        object.__setattr__(self, "event_queue", event_queue)
        # key -> {"type": ..., "settable": [...]}, from the job command.
        # None when the engine sent no spec (it always does since the
        # property update shipped; kept so the value path never depends
        # on it).
        specs = None
        if components is not None:
            specs = {c["key"]: c for c in components}
        object.__setattr__(self, "_components", specs)
        # The job connection's engine -> Python side. None outside a
        # worker connection (tests, introspection): `wait()` refuses.
        object.__setattr__(self, "_engine", engine)
        object.__setattr__(self, "_panel", panel)
        # The panel's button names, from the job command.
        object.__setattr__(self, "_events", list(events or []))
        # Undeclared keys already warned about as a likely typo.
        object.__setattr__(self, "_warned", set())

    def __setattr__(self, name: str, value: Any):
        if name in ("job_id", "event_queue", "_components", "_engine"):
            object.__setattr__(self, name, value)
            return
        specs = object.__getattribute__(self, "_components")
        warned = object.__getattribute__(self, "_warned")
        if specs and name not in specs and name not in warned:
            # A panel value by design, or a typo of a component key: the
            # near miss is worth a line, once.
            close = difflib.get_close_matches(name, list(specs), n=1)
            if close:
                warned.add(name)
                print(
                    f"ui.{name}: no component '{name}' in this phase (did you mean "
                    f"'{close[0]}'?); the value only shows where a panel binds ui.{name}",
                    file=sys.stderr,
                )

        self.event_queue.put(
            {
                "type": "ui_update",
                "job_id": self.job_id,
                "action": "set_value",
                "data": {"id": name, "value": value},
            }
        )

    def set(self, key: str, **properties: Any) -> None:
        """Change properties of the component `key` declared in the
        procedure YAML. Several properties in one call reach the screen
        together. `None` clears a property; an omitted one is untouched.

            ui.set("mo", options=[{"value": str(o.id), "label": o.name} for o in orders])
            ui.set("voltage", min=11.5, max=12.5, suffix="V")
            ui.set("note", label="Comments", placeholder=None)
        """
        if not isinstance(key, str) or not key:
            raise TypeError("ui.set: the first argument is the component key (a string)")
        if not properties:
            raise TypeError(
                f"ui.set('{key}'): no property given, e.g. ui.set('{key}', options=[...])"
            )
        specs = object.__getattribute__(self, "_components")
        settable = None
        if specs is not None:
            spec = specs.get(key)
            if spec is None:
                keys = ", ".join(specs) or "none"
                raise KeyError(
                    f"ui.set('{key}'): no component {key} (have: {keys}); "
                    f"declare it under `ui.components` in procedure.yaml"
                )
            settable = spec.get("settable")

        normalized = {}
        for name, value in properties.items():
            if settable is not None and name not in settable:
                comp_type = specs[key].get("type", "component")
                raise ValueError(
                    f"ui.set('{key}'): '{key}' is a {comp_type} and has no '{name}' property "
                    f"that a phase can set (settable: {', '.join(settable)})"
                )
            shape = _SETTABLE_SHAPES.get(name)
            if shape is None:
                raise ValueError(
                    f"ui.set('{key}'): unknown property '{name}' "
                    f"(known: {', '.join(_SETTABLE_SHAPES)})"
                )
            normalized[name] = shape(name, value)

        self.event_queue.put(
            {
                "type": "ui_update",
                "job_id": self.job_id,
                "action": "set_properties",
                "data": {"id": key, "properties": normalized},
            }
        )

    def pressed(self, name: str) -> int:
        """How many times the operator pressed the panel's
        `<button name="...">` since the last call for that name (0 when
        not). Never blocks: read it in the phase's loop.

            while not done():
                measurements.ias = probe.read()
                if ui.pressed("tare"):
                    probe.tare()

        Raises `ValueError` for a name the phase's panel has no button for.
        """
        events = object.__getattribute__(self, "_events")
        if name not in events:
            have = ", ".join(events) or "none (the phase's ui.panel has no <button name>)"
            close = difflib.get_close_matches(name, events, n=1)
            hint = f" Did you mean '{close[0]}'?" if close else ""
            raise ValueError(f"ui.pressed('{name}'): no panel button '{name}' (have: {have}).{hint}")
        engine = object.__getattribute__(self, "_engine")
        return engine.take_presses(name) if engine is not None else 0

    def wait(self) -> Dict[str, Any]:
        """Block until the operator submits this phase's screen, then
        return the answer: one entry per input component, typed per
        component (number and slider -> float or None, switch ->
        bool, multiselect and checklist -> list of str, select and radio ->
        str or None, text -> str with its prefix and suffix).

            while True:
                answer = ui.wait()
                if answer["done"]:
                    break  # the operator confirmed the reading on screen
                dut.set_offset(answer["offset"])
                ui.reading = f"{dut.read():.3f} V"

        The first call answers the screen already shown at phase start;
        each later call shows it again with its current values (the last
        answer, then any `ui.<key> = ...` written since) and every
        `ui.set` applied. A screen that only displays gets a Continue
        button for the call. Once a phase has called `wait()`, it ends
        without asking for another Continue, and `bind:` records the last
        answer only.

        Raises `UiWaitCancelled` when the answer can never come (run or
        slot stopped, phase timed out, prompt cancelled by the operator),
        and `RuntimeError` for a wait that should not be made (no screen,
        another thread of the phase already waiting). There is no timeout
        argument: the phase timeout bounds the wait.
        """
        engine = object.__getattribute__(self, "_engine")
        if engine is None:
            raise RuntimeError("ui.wait() only works inside a phase run by the TofuPilot engine")
        specs = object.__getattribute__(self, "_components")
        if specs is not None and not specs and not object.__getattribute__(self, "_panel"):
            raise RuntimeError(
                "ui.wait(): phase has no `ui.components` or `ui.panel` in procedure.yaml"
            )
        wait_id, slot = engine.open()
        self.event_queue.put({"type": "ui_wait", "job_id": self.job_id, "wait_id": wait_id})
        message = slot.get()
        kind = message.get("type")
        if kind == "UiAnswer":
            engine.mark_answered(wait_id)
            return dict(message.get("values") or {})
        if kind == "UiRefused":
            # A wait the phase should never have made (no screen, a second
            # thread already waiting): an authoring error, not a cancel.
            raise RuntimeError(f"ui.wait(): {message.get('reason') or 'refused by the engine'}")
        raise UiWaitCancelled(message.get("reason") or "ui.wait() was cancelled")


class _BindingError(TypeError):
    """A phase parameter nothing can fill: reported without a traceback,
    the frames would be the worker's."""


class _PhaseResultException(Exception):
    """Internal exception for phase result control flow."""

    def __init__(self, result_value: str, message: str = ""):
        self.result_value = result_value
        self.message = message
        super().__init__(message)


class Phase:
    """Phase execution control."""

    @staticmethod
    def fail(message: str = "") -> None:
        raise _PhaseResultException(PhaseResult.FAIL, message)

    @staticmethod
    def skip(message: str = "") -> None:
        raise _PhaseResultException(PhaseResult.SKIP, message)

    @staticmethod
    def retry(message: str = "") -> None:
        raise _PhaseResultException(PhaseResult.RETRY, message)

    @staticmethod
    def stop(message: str = "") -> None:
        raise _PhaseResultException(PhaseResult.STOP, message)

    def __repr__(self):
        return f"<Phase(fail, skip, retry, stop)>"


class ValidatorLevel:
    CRITICAL = "critical"
    ALERT = "alert"
    NOTICE = "notice"


class ValidatorOutcome:
    PASS = "PASS"
    FAIL = "FAIL"
    UNSET = "UNSET"


ValidatorOutcomeType = Literal["PASS", "FAIL", "UNSET"]
ValidatorLevelType = Literal["critical", "alert", "notice"]


class PhaseResult:
    """Phase result values matching Rust PhaseResult enum"""
    CONTINUE = "Continue"
    RETRY = "Retry"
    SKIP = "Skip"
    STOP = "Stop"
    FAIL = "Fail"


PhaseResultType = Literal["Continue", "Retry", "Skip", "Stop", "Fail"]


class Axis:
    """Axis specification for multi-dimensional data matching Rust AxisSpec"""

    def __init__(
        self,
        data: List[Union[float, int, str]],
        aggregations: Optional[List["Aggregation"]] = None,
        validators: Optional[List["Validator"]] = None,
    ):
        # A numpy array or numpy scalars in a list record like a plain list.
        data = _plain_json_value(data)
        if not isinstance(data, list):
            raise ValueError(f"Axis data must be a list, got {type(data)}")

        self.data = data
        self.aggregations = aggregations or []
        self.validators = validators or []

    def to_dict(self) -> Dict[str, Any]:
        result = {"data": _plain_json_value(self.data)}
        if hasattr(self, "unit") and self.unit is not None:
            result["unit"] = self.unit
        if hasattr(self, "legend") and self.legend is not None:
            result["legend"] = self.legend
        if self.aggregations:
            result["aggregations"] = [
                a.to_dict() if hasattr(a, "to_dict") else a for a in self.aggregations
            ]
        if self.validators:
            result["validators"] = [
                v.to_dict() if hasattr(v, "to_dict") else v for v in self.validators
            ]
        return result


class MultiDim:
    """Multi-dimensional measurement specification matching Rust MultiDimensionalSpec"""

    def __init__(
        self,
        x_axis: Axis,
        y_axis: List[Axis],
    ):
        if not isinstance(x_axis, Axis):
            raise ValueError(f"x_axis must be an Axis instance, got {type(x_axis)}")

        if not isinstance(y_axis, list):
            raise ValueError(f"y_axis must be a list, got {type(y_axis)}")

        for i, axis in enumerate(y_axis):
            if not isinstance(axis, Axis):
                raise ValueError(
                    f"y_axis[{i}] must be an Axis instance, got {type(axis)}"
                )

        x_length = len(x_axis.data)
        for i, axis in enumerate(y_axis):
            if len(axis.data) != x_length:
                raise ValueError(
                    f"y_axis[{i}] data length ({len(axis.data)}) must match "
                    f"x_axis data length ({x_length})"
                )

        self.x_axis = x_axis
        self.y_axis = y_axis

    def to_dict(self) -> Dict[str, Any]:
        result = {
            "x_axis": self.x_axis.to_dict(),
            "y_axis": [axis.to_dict() for axis in self.y_axis],
        }
        return result


ScalarValueType = Union[float, int, str, bool]
MeasurementValueType = Union[float, int, str, bool, List, Dict, MultiDim]


class Validator:
    """Validator specification for Python usage"""

    def __init__(
        self,
        level: ValidatorLevelType = None,
        outcome: ValidatorOutcomeType = None,
        operator: str = None,
    ):
        if level is not None and level not in [
            ValidatorLevel.CRITICAL,
            ValidatorLevel.ALERT,
            ValidatorLevel.NOTICE,
        ]:
            raise ValueError(f"Invalid validator level: {level}")
        self.level = level

        if outcome is not None and outcome not in [
            ValidatorOutcome.PASS,
            ValidatorOutcome.FAIL,
            ValidatorOutcome.UNSET,
        ]:
            raise ValueError(f"Invalid validator outcome: {outcome}")
        self.outcome = outcome

        self.operator = operator

    def to_dict(self) -> Dict[str, Any]:
        result = {}
        if self.level is not None:
            result["level"] = self.level
        if self.outcome is not None:
            result["outcome"] = self.outcome
        if self.operator is not None:
            result["operator"] = self.operator
        return result


class Aggregation:
    """Aggregation specification for Python usage"""

    def __init__(
        self,
        aggregation_type: str,
        value: Optional[MeasurementValueType] = None,
        outcome: Optional[ValidatorOutcomeType] = None,
        validators: Optional[List[Validator]] = None,
    ):
        self.aggregation_type = aggregation_type
        if isinstance(value, (dict, list, tuple)):
            # The server stores an aggregation as one numeric, string or
            # boolean measurement; a structured value has nowhere to go
            # and would fail the whole upload.
            raise ValueError(
                f"Aggregation '{aggregation_type}' value must be a number, string or "
                f"boolean, got {type(value).__name__}"
            )
        self.value = value

        if outcome is not None and outcome not in [
            ValidatorOutcome.PASS,
            ValidatorOutcome.FAIL,
            ValidatorOutcome.UNSET,
        ]:
            raise ValueError(f"Invalid aggregation outcome: {outcome}")
        self.outcome = outcome

        if validators:
            for v in validators:
                if not isinstance(v, Validator):
                    raise ValueError(
                        f"Validators must be Validator instances, got {type(v)}"
                    )
        self.validators = validators or []

    def to_dict(self) -> Dict[str, Any]:
        result = {"type": self.aggregation_type}
        if self.outcome is not None:
            result["outcome"] = self.outcome
        if self.value is not None:
            result["value"] = self.value
        if self.validators:
            result["validators"] = [v.to_dict() for v in self.validators]
        return result


class Measurement:
    """Measurement for Python usage"""

    def __init__(
        self,
        name: str,
        value: Optional[MeasurementValueType] = None,
        validators: Optional[List[Validator]] = None,
        aggregations: Optional[List[Aggregation]] = None,
    ):
        if not isinstance(name, str) or not name:
            raise ValueError("Measurement name must be a non-empty string")
        self.name = name

        if value is not None:
            if isinstance(value, MultiDim):
                self.value = value.to_dict()
                self.is_multi_dimensional = True
            elif isinstance(value, (float, int, str, bool, list, dict)):
                self.value = value
                self.is_multi_dimensional = (
                    isinstance(value, dict) and "x_axis" in value
                )
            else:
                raise ValueError(f"Invalid measurement value type: {type(value)}")
        else:
            self.value = None
            self.is_multi_dimensional = False

        if validators:
            for v in validators:
                if not isinstance(v, Validator):
                    raise ValueError(
                        f"Validators must be Validator instances, got {type(v)}"
                    )
        self.validators = validators or []

        if aggregations:
            for a in aggregations:
                if not isinstance(a, Aggregation):
                    raise ValueError(
                        f"Aggregations must be Aggregation instances, got {type(a)}"
                    )
        self.aggregations = aggregations or []

        if hasattr(self, "unit") or hasattr(self, "docstring"):
            warnings.warn(
                "Python should only set measurement values and outcomes. Define metadata in YAML.",
                UserWarning,
            )

    def to_dict(self) -> Dict[str, Any]:
        result = {
            "name": self.name,
            "value": self.value,
            "timestamp": time.strftime("%H:%M:%S.%f")[:-3],
        }
        if self.validators:
            result["validators"] = [v.to_dict() for v in self.validators]
        if self.aggregations:
            result["aggregations"] = [a.to_dict() for a in self.aggregations]
        return result


# Server rules on unit identity fields (serial, part, revision, batch,
# sub-unit serials): `runs.create` rejects anything longer than 60
# characters or outside this charset. Mirrored here so a phase that sets
# one learns it at the assignment, not after the run has been refused.
_UNIT_FIELD_MAX_CHARS = 60
_UNIT_FIELD_CHARSET = re.compile(r"^[a-zA-Z0-9_.:+-]+$")
# `runs.create` caps measurement names at 100 characters.
_MEASUREMENT_NAME_MAX_CHARS = 100


def _plain_json_value(value):
    """numpy scalars and arrays (anything with a `.tolist()`) as plain
    Python values, through lists, tuples and dicts. A numpy scalar is
    not JSON, and used to fail the whole phase at its result line."""
    if value is None or isinstance(value, (str, bool, int, float)):
        return value
    if isinstance(value, dict):
        return {k: _plain_json_value(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_plain_json_value(v) for v in value]
    tolist = getattr(value, "tolist", None)
    if callable(tolist):
        try:
            return _plain_json_value(tolist())
        except Exception:
            return value
    return value


def _short_repr(value) -> str:
    shown = repr(value)
    return shown[:200] + "..." if len(shown) > 200 else shown


def _check_recordable(value, what: str):
    """Raise in the phase, at the line that records `value`, if it cannot
    reach the engine. NaN and Infinity are not JSON: the engine refused
    the whole result line, and every measurement and log went with it."""
    try:
        json.dumps(value, allow_nan=False)
    except ValueError:
        raise ValueError(
            f"{what} must be finite: NaN and Infinity cannot be recorded, "
            f"got {_short_repr(value)}"
        ) from None
    except TypeError as e:
        raise TypeError(
            f"{what} cannot be recorded: {e}. Record a number, string, bool, list or dict"
        ) from None


def _format_ms(ms) -> str:
    """`30000` -> `30s`, `90500` -> `1m30s500ms`: the engine's duration form."""
    ms = int(ms)
    if ms < 1000:
        return f"{ms}ms"
    hours, rest = divmod(ms, 3_600_000)
    minutes, rest = divmod(rest, 60_000)
    secs, millis = divmod(rest, 1000)
    parts = [f"{v}{u}" for v, u in ((hours, "h"), (minutes, "m"), (secs, "s"), (millis, "ms")) if v]
    return "".join(parts)


def _validate_identity_value(field: str, value: str) -> str:
    if not isinstance(value, str):
        raise ValueError(f"{field} must be a string, got {type(value).__name__}")
    trimmed = value.strip()
    if not trimmed:
        raise ValueError(f"{field}: empty")
    if len(trimmed) > _UNIT_FIELD_MAX_CHARS:
        raise ValueError(
            f"{field}: expected at most {_UNIT_FIELD_MAX_CHARS} characters, got {len(trimmed)}"
        )
    if not _UNIT_FIELD_CHARSET.match(trimmed):
        raise ValueError(f"{field}: invalid `{trimmed}`: expected A-Z a-z 0-9 _ . : + -")
    return trimmed


class SubUnitsDict(dict):
    """Dict of sub-unit serial numbers with attribute access (read/write)."""

    def __init__(self, sub_units_dict: Dict[str, str]):
        super().__init__(sub_units_dict)
        object.__setattr__(self, '_attr_to_label', {})
        for label in sub_units_dict.keys():
            attr_name = self._sanitize_label(label)
            if attr_name:
                self._attr_to_label[attr_name] = label

    @staticmethod
    def _sanitize_label(label: str) -> Optional[str]:
        import re
        name = label.lower().replace(" ", "_").replace("-", "_")
        name = re.sub(r'[^a-z0-9_]', '', name)
        if name and name[0].isdigit():
            name = f"_{name}"
        return name if name else None

    def __getitem__(self, key: str) -> str:
        if key in self.keys():
            return super().__getitem__(key)
        key_lower = key.lower()
        for label in self.keys():
            if label.lower() == key_lower:
                return super().__getitem__(label)
        raise KeyError(f"No sub-unit named '{key}'")

    def __setitem__(self, key: str, value: str):
        if key in self.keys():
            super().__setitem__(key, _validate_identity_value(f"sub-unit {key}", value))
            return
        key_lower = key.lower()
        for label in self.keys():
            if label.lower() == key_lower:
                super().__setitem__(label, _validate_identity_value(f"sub-unit {label}", value))
                return
        raise KeyError(f"No sub-unit named '{key}'. Cannot add new sub-units at runtime.")

    def __getattr__(self, name: str) -> str:
        if name.startswith('__') or name == '_attr_to_label':
            return object.__getattribute__(self, name)
        attr_to_label = object.__getattribute__(self, '_attr_to_label')
        if name in attr_to_label:
            return self[attr_to_label[name]]
        raise AttributeError(f"No sub-unit named '{name}'")

    def __setattr__(self, name: str, value: str):
        if name.startswith('__') or name == '_attr_to_label':
            object.__setattr__(self, name, value)
            return
        attr_to_label = object.__getattribute__(self, '_attr_to_label')
        if name in attr_to_label:
            self[attr_to_label[name]] = value
        else:
            raise AttributeError(f"No sub-unit named '{name}'. Cannot add new sub-units at runtime.")


# Mirrors the server-side metadata constraints (MetadataInputSchema):
# keys <= 40 chars matching this charset, <= 50 keys per entity,
# values limited to string | number | bool (strings <= 50000 chars).
_METADATA_KEY_RE = re.compile(r"[a-zA-Z0-9_.:+-]+")
_METADATA_MAX_KEYS = 50
_METADATA_MAX_KEY_LEN = 40
_METADATA_MAX_VALUE_LEN = 50000


class Inputs:
    """The resolved `inputs:` of the sequence a phase belongs to.

    Read as attributes (`inputs.target_voltage`) or items
    (`inputs["target_voltage"]`). Read-only: the values were fixed by the
    call's `with:` and the file's defaults when the procedure loaded.
    Outside a sequence it is empty, and any read names what is missing.
    """

    def __init__(self, values):
        object.__setattr__(self, "_values", dict(values))

    def __getattribute__(self, name):
        # A declared input wins over the object's own names: `items`,
        # `keys`, `get` or `_values` are ordinary input names, and a class
        # attribute would otherwise shadow them (`inputs.items` a method).
        if not name.startswith("__"):
            values = object.__getattribute__(self, "_values")
            if name in values:
                return values[name]
        return object.__getattribute__(self, name)

    def _missing(self, name):
        values = object.__getattribute__(self, "_values")
        if values:
            declared = ", ".join(sorted(values))
            return AttributeError(
                f"inputs has no '{name}'. This sequence declares: {declared}."
            )
        return AttributeError(
            f"inputs has no '{name}': this phase is not part of a sequence, or "
            f"its sequence file declares no inputs:"
        )

    def __getattr__(self, name):
        values = object.__getattribute__(self, "_values")
        if name in values:
            return values[name]
        raise object.__getattribute__(self, "_missing")(name)

    def __setattr__(self, name, value):
        raise AttributeError(
            f"inputs are read-only: '{name}' is set by the procedure's with: or the sequence's default"
        )

    def __getitem__(self, name):
        values = object.__getattribute__(self, "_values")
        if name in values:
            return values[name]
        raise KeyError(str(object.__getattribute__(self, "_missing")(name)))

    def __contains__(self, name):
        return name in object.__getattribute__(self, "_values")

    def __iter__(self):
        return iter(object.__getattribute__(self, "_values"))

    def __len__(self):
        return len(object.__getattribute__(self, "_values"))

    def get(self, name, default=None):
        return object.__getattribute__(self, "_values").get(name, default)

    def keys(self):
        return object.__getattribute__(self, "_values").keys()

    def items(self):
        return object.__getattribute__(self, "_values").items()

    def __repr__(self):
        return f"Inputs({object.__getattribute__(self, '_values')!r})"


class _DeclaredValues:
    """Read-only values by key, read as attributes or items. The base of
    `Config` and `Secrets`, modelled on `Inputs`: a declared key wins over
    the object's own names, and a missing one names what is declared."""

    _kind = "values"
    _section = "values:"

    def __init__(self, values):
        object.__setattr__(self, "_values", dict(values or {}))

    def __getattribute__(self, name):
        if not name.startswith("__"):
            values = object.__getattribute__(self, "_values")
            if name in values:
                return values[name]
        return object.__getattribute__(self, name)

    def _missing(self, name):
        values = object.__getattribute__(self, "_values")
        kind = object.__getattribute__(self, "_kind")
        section = object.__getattribute__(self, "_section")
        if values:
            declared = ", ".join(sorted(values))
            return AttributeError(
                f"{kind} has no '{name}'. Declared for this phase: {declared}; "
                f"add '{name}' under {section} in procedure.yaml (and in the "
                f"sequence file, for a phase of a sequence)"
            )
        return AttributeError(
            f"{kind} has no '{name}': declare it under {section} in procedure.yaml "
            f"(and in the sequence file, for a phase of a sequence)"
        )

    def __getattr__(self, name):
        values = object.__getattribute__(self, "_values")
        if name in values:
            return values[name]
        raise object.__getattribute__(self, "_missing")(name)

    def __setattr__(self, name, value):
        kind = object.__getattribute__(self, "_kind")
        raise AttributeError(
            f"{kind}.{name} is read-only: it is set for the station or the procedure, not by a phase"
        )

    def __getitem__(self, name):
        values = object.__getattribute__(self, "_values")
        if name in values:
            return values[name]
        raise KeyError(str(object.__getattribute__(self, "_missing")(name)))

    def __contains__(self, name):
        return name in object.__getattribute__(self, "_values")

    def __iter__(self):
        return iter(object.__getattribute__(self, "_values"))

    def __len__(self):
        return len(object.__getattribute__(self, "_values"))

    def get(self, name, default=None):
        value = object.__getattribute__(self, "_values").get(name)
        return default if value is None else value

    def keys(self):
        return object.__getattribute__(self, "_values").keys()

    def items(self):
        return object.__getattribute__(self, "_values").items()


class Config(_DeclaredValues):
    """The `config:` a phase reads (`config.MES_URL`), typed as declared. A
    declared key without a value reads as None."""

    _kind = "config"
    _section = "config:"

    def __repr__(self):
        return f"Config({object.__getattribute__(self, '_values')!r})"


class Secrets(_DeclaredValues):
    """The `secrets:` a phase reads (`secrets.MES_TOKEN`). The repr shows
    the keys only, so printing the object never prints a secret."""

    _kind = "secrets"
    _section = "secrets:"

    def __repr__(self):
        values = object.__getattribute__(self, "_values")
        shown = ", ".join(
            f"{key}={'None' if value is None else '•••'}" for key, value in sorted(values.items())
        )
        return f"Secrets({shown})"

    __str__ = __repr__


class Metadata(dict):
    """Validated key/value metadata (string | number | bool values).

    Validation happens at assignment so authors get a clear error at the
    faulty line instead of a silently dropped upload field.
    """

    def __init__(self, owner: str):
        super().__init__()
        object.__setattr__(self, "_owner", owner)

    def _validate(self, key, value):
        owner = object.__getattribute__(self, "_owner")
        if not isinstance(key, str):
            raise TypeError(
                f"{owner}.metadata keys must be strings, got {type(key).__name__}"
            )
        if len(key) > _METADATA_MAX_KEY_LEN:
            raise ValueError(
                f"{owner}.metadata key '{key[:_METADATA_MAX_KEY_LEN]}…' exceeds "
                f"{_METADATA_MAX_KEY_LEN} characters"
            )
        # fullmatch, not match with $: "$" would accept a trailing newline
        # that the server-side regex rejects at upload.
        if not _METADATA_KEY_RE.fullmatch(key):
            raise ValueError(
                f"{owner}.metadata key '{key}' is invalid: only letters, digits "
                f"and _ . : + - are allowed"
            )
        if not isinstance(value, (str, int, float, bool)):
            raise TypeError(
                f"{owner}.metadata['{key}'] must be a string, number, or bool, "
                f"got {type(value).__name__}"
            )
        if isinstance(value, str) and len(value) > _METADATA_MAX_VALUE_LEN:
            raise ValueError(
                f"{owner}.metadata['{key}'] exceeds {_METADATA_MAX_VALUE_LEN} "
                f"characters ({len(value)})"
            )
        # json.dumps would emit bare NaN/Infinity, which the Rust JSON parser
        # rejects — dropping the whole metadata bundle. Reject at assignment.
        if isinstance(value, float) and not math.isfinite(value):
            raise ValueError(
                f"{owner}.metadata['{key}'] must be a finite number, got {value!r}"
            )

    def __setitem__(self, key, value):
        owner = object.__getattribute__(self, "_owner")
        self._validate(key, value)
        if key not in self and len(self) >= _METADATA_MAX_KEYS:
            raise ValueError(
                f"{owner}.metadata is limited to {_METADATA_MAX_KEYS} keys"
            )
        super().__setitem__(key, value)

    def update(self, *args, **kwargs):
        # Validate everything first, then commit — a bulk write never
        # applies partially.
        owner = object.__getattribute__(self, "_owner")
        items = dict(*args, **kwargs)
        for k, v in items.items():
            self._validate(k, v)
        new_keys = sum(1 for k in items if k not in self)
        if len(self) + new_keys > _METADATA_MAX_KEYS:
            raise ValueError(
                f"{owner}.metadata is limited to {_METADATA_MAX_KEYS} keys"
            )
        for k, v in items.items():
            dict.__setitem__(self, k, v)

    def __ior__(self, other):
        # dict's C-level |= skips the overridden __setitem__; route it
        # through update() so validation still runs.
        self.update(other)
        return self

    def setdefault(self, key, default=None):
        if key in self:
            return self[key]
        if default is None:
            # None is not a valid metadata value; unlike plain dict we
            # don't insert it — just report the key as absent.
            return None
        self[key] = default
        return self[key]


class _MetadataOwnerMixin:
    """Guards `x.metadata = {...}` reassignment: replacing the Metadata
    dict with a plain dict would bypass all validation, so the setter
    re-validates by routing the new content through Metadata.update()."""

    @property
    def metadata(self):
        return self._metadata

    @metadata.setter
    def metadata(self, value):
        owner = object.__getattribute__(self, "_metadata")._owner
        fresh = Metadata(owner)
        fresh.update(dict(value))
        object.__setattr__(self, "_metadata", fresh)


class Unit(_MetadataOwnerMixin):
    _IDENTITY_FIELDS = ("serial_number", "part_number", "revision_number", "batch_number")

    def __repr__(self):
        fields = ", ".join(
            f"{f}={getattr(self, f, None)!r}"
            for f in self._IDENTITY_FIELDS
            if getattr(self, f, None) is not None
        )
        return f"Unit({fields})"

    def __setattr__(self, name: str, value):
        if name in self._IDENTITY_FIELDS and value is not None:
            value = _validate_identity_value(name, value)
        object.__setattr__(self, name, value)

    def __init__(self, initial_metadata=None):
        object.__setattr__(self, "_metadata", Metadata("unit"))
        self.serial_number: Optional[str] = None
        self.batch_number: Optional[str] = None
        self.part_number: Optional[str] = None
        self.revision_number: Optional[str] = None
        self.sub_units: Optional[SubUnitsDict] = None
        if initial_metadata:
            # Operator-entered identify-form values, threaded in via the
            # job command so phases can read them. Seeded WITHOUT Python
            # validation: they were already validated at identify time,
            # and a validation raise here would abort the phase before
            # its own try/except with an opaque worker error.
            for k, v in initial_metadata.items():
                dict.__setitem__(self._metadata, k, v)


class _AggregationsReadProxy:
    """Read-only proxy for accessing aggregation values from cross-phase results."""

    def __init__(self, aggregations_list: list):
        lookup = {}
        for agg in aggregations_list:
            agg_type = agg.get("type", "")
            if "value" in agg and agg["value"] is not None:
                val = agg["value"]
                if isinstance(val, dict):
                    for _, v in val.items():
                        val = v
                        break
                lookup[agg_type] = val
        object.__setattr__(self, "_lookup", lookup)

    def __getattr__(self, name: str):
        if name.startswith("_"):
            raise AttributeError(f"'_AggregationsReadProxy' object has no attribute '{name}'")
        lookup = object.__getattribute__(self, "_lookup")
        if name in lookup:
            return lookup[name]
        raise AttributeError(f"No aggregation '{name}' found")

    def __repr__(self):
        return f"<_AggregationsReadProxy({list(self._lookup.keys())})>"


class _MeasurementReadProxy:
    """Read-only proxy wrapping a measurement value with aggregation access."""

    def __init__(self, value, aggregations: list):
        object.__setattr__(self, "_value", value)
        object.__setattr__(self, "_aggregations", aggregations)

    @property
    def aggregations(self):
        return _AggregationsReadProxy(object.__getattribute__(self, "_aggregations"))

    def _unwrap(self, other):
        if isinstance(other, (_MeasurementReadProxy, _MeasurementValueProxy)):
            return object.__getattribute__(other, "_value")
        return other

    def __repr__(self):
        return repr(self._value)

    def __str__(self):
        return str(self._value)

    def __bool__(self):
        return bool(self._value)

    def __hash__(self):
        return hash(self._value)

    def __float__(self):
        return float(self._value)

    def __int__(self):
        return int(self._value)

    def __index__(self):
        return self._value.__index__()

    def __eq__(self, other):
        return self._value == self._unwrap(other)

    def __ne__(self, other):
        return self._value != self._unwrap(other)

    def __lt__(self, other):
        return self._value < self._unwrap(other)

    def __le__(self, other):
        return self._value <= self._unwrap(other)

    def __gt__(self, other):
        return self._value > self._unwrap(other)

    def __ge__(self, other):
        return self._value >= self._unwrap(other)

    def __add__(self, other):
        return self._value + self._unwrap(other)

    def __radd__(self, other):
        return other + self._value

    def __sub__(self, other):
        return self._value - self._unwrap(other)

    def __rsub__(self, other):
        return other - self._value

    def __mul__(self, other):
        return self._value * self._unwrap(other)

    def __rmul__(self, other):
        return other * self._value

    def __truediv__(self, other):
        return self._value / self._unwrap(other)

    def __rtruediv__(self, other):
        return other / self._value

    def __neg__(self):
        return -self._value

    def __abs__(self):
        return abs(self._value)

    def __round__(self, ndigits=None):
        return round(self._value, ndigits)

    def __len__(self):
        return len(self._value)

    def __iter__(self):
        return iter(self._value)

    def __getitem__(self, key):
        return self._value[key]

    def __contains__(self, item):
        return item in self._value


class _PhaseResultsMeasurements:
    """Read-only proxy for accessing measurements from a completed phase with MDM support."""

    def __init__(self, data: dict):
        object.__setattr__(self, "_data", data)

    def __getattr__(self, name: str):
        if name.startswith("_"):
            raise AttributeError(f"'_PhaseResultsMeasurements' object has no attribute '{name}'")
        if name in self._data:
            value = self._data[name]
            if isinstance(value, dict) and "__value__" in value:
                raw_value = value["__value__"]
                aggregations = value.get("__aggregations__", [])
                return _MeasurementReadProxy(raw_value, aggregations)
            if isinstance(value, dict) and ("x_axis" in value or "y_axis" in value):
                return _MDMReadProxy(value)
            return value
        raise AttributeError(f"No measurement '{name}' found")

    def __repr__(self):
        return f"<_PhaseResultsMeasurements({list(self._data.keys())})>"


class PhaseResults:
    """Read-only access to a completed phase's measurements."""

    def __init__(self, function_name: str, data: dict):
        object.__setattr__(self, "_function_name", function_name)
        object.__setattr__(self, "_data", data)

    def __setattr__(self, name, value):
        raise AttributeError("PhaseResults is read-only")

    def __getattr__(self, name: str):
        if name.startswith("_"):
            raise AttributeError(f"'PhaseResults' object has no attribute '{name}'")
        if name == "measurements":
            return _PhaseResultsMeasurements(self._data)
        if name in self._data:
            return self._data[name]
        raise AttributeError(
            f"Phase '{self._function_name}' has no measurement '{name}'"
        )

    # `channels["ch_1"]` and `"ch_1" in channels`, as the phase that
    # recorded them with `measurements[...]` wrote them.
    def __getitem__(self, name: str):
        if name in self._data:
            return self._data[name]
        raise KeyError(f"Phase '{self._function_name}' has no measurement '{name}'")

    def __contains__(self, name) -> bool:
        return name in self._data

    def __iter__(self):
        return iter(self._data)

    def __repr__(self):
        return f"<PhaseResults({self._function_name}: {list(self._data.keys())})>"


class _RunSection:
    """One read-only group of the run context (`run.slot`, `run.station`,
    ...): its fields as attributes, None where the host does not know them."""

    def __init__(self, name: str, fields, values):
        values = values or {}
        object.__setattr__(self, "_name", name)
        object.__setattr__(
            self, "_values", {field: values.get(field) for field in fields}
        )

    def __getattr__(self, field):
        values = object.__getattribute__(self, "_values")
        if field in values:
            return values[field]
        name = object.__getattribute__(self, "_name")
        raise AttributeError(
            f"run.{name} has no '{field}' (fields: {', '.join(values)})"
        )

    def __setattr__(self, field, value):
        name = object.__getattribute__(self, "_name")
        raise AttributeError(f"run.{name} is read-only")

    def __repr__(self):
        name = object.__getattribute__(self, "_name")
        values = object.__getattribute__(self, "_values")
        inner = ", ".join(f"{k}={v!r}" for k, v in values.items())
        return f"run.{name}({inner})"


class Run(_MetadataOwnerMixin):
    """Run information and run-level metadata.

    Read-only: `slot` (key, name), `station` (id, name), `procedure` (id,
    name, version), `deployment` (id, environment, commit, branch) and
    `retry` (count, limit), the same paths `{run.*}` markers read in the
    YAML. `slot_id`, `retry_count`, `retry_limit` and `job_id` are the
    older names, kept. `operated_by` and `metadata` are writable.
    """

    def __init__(
        self,
        slot_id: str,
        job_id: str,
        retry_count: int,
        retry_limit: int,
        context: Optional[Dict[str, Any]] = None,
    ):
        object.__setattr__(self, "_metadata", Metadata("run"))
        context = context or {}
        slot = context.get("slot") or (
            None if slot_id in (None, "<shared>") else {"key": slot_id, "name": slot_id}
        )
        object.__setattr__(self, "_job_id", job_id)
        object.__setattr__(self, "_slot_id", slot_id)
        object.__setattr__(self, "_slot", _RunSection("slot", ("key", "name"), slot))
        object.__setattr__(
            self, "_station", _RunSection("station", ("id", "name"), context.get("station"))
        )
        object.__setattr__(
            self,
            "_procedure",
            _RunSection("procedure", ("id", "name", "version"), context.get("procedure")),
        )
        object.__setattr__(
            self,
            "_deployment",
            _RunSection(
                "deployment",
                ("id", "environment", "commit", "branch"),
                context.get("deployment"),
            ),
        )
        object.__setattr__(
            self,
            "_retry",
            _RunSection("retry", ("count", "limit"), {"count": retry_count, "limit": retry_limit}),
        )
        # Operator attributed to the run, email or free-text name (uploaded as
        # `operated_by`). Seeded from the identify step; writable so a
        # phase can set/correct it (e.g. read from a badge scanner).
        self.operated_by: Optional[str] = None

    _READ_ONLY = (
        "slot",
        "station",
        "procedure",
        "deployment",
        "retry",
        "slot_id",
        "job_id",
        "retry_count",
        "retry_limit",
    )

    def __setattr__(self, name, value):
        if name in Run._READ_ONLY:
            raise AttributeError(f"run.{name} is read-only")
        object.__setattr__(self, name, value)

    @property
    def slot(self):
        return self._slot

    @property
    def station(self):
        return self._station

    @property
    def procedure(self):
        return self._procedure

    @property
    def deployment(self):
        return self._deployment

    @property
    def retry(self):
        return self._retry

    @property
    def slot_id(self):
        """The older name of `run.slot.key` (`<shared>` for a shared job)."""
        return self._slot_id

    @property
    def job_id(self):
        return self._job_id

    @property
    def retry_count(self):
        """The older name of `run.retry.count`."""
        return self._retry.count

    @property
    def retry_limit(self):
        """The older name of `run.retry.limit`."""
        return self._retry.limit


class Attachments:
    def __repr__(self):
        return "Attachments()"

    def __init__(self, job_id: str, event_queue: EventQueue):
        self.job_id = job_id
        self.event_queue = event_queue

    def file(self, file_path: str, attachment_name: str = None):
        if not os.path.isabs(file_path):
            procedure_dir = Path(os.getcwd())
            file_path = str(procedure_dir / file_path)

        if attachment_name is None:
            attachment_name = os.path.basename(file_path)

        self.event_queue.put(
            {
                "type": "attach_file",
                "job_id": self.job_id,
                "source_path": file_path,
                "attachment_name": attachment_name,
            }
        )

    def data(self, data: bytes, attachment_name: str):
        self.event_queue.put(
            {
                "type": "attach_data",
                "job_id": self.job_id,
                "data": base64.b64encode(data).decode("utf-8"),
                "attachment_name": attachment_name,
            }
        )


class _YAxisAggregationsWriteProxy:
    def __init__(self, builder: "_MDMBuilder", axis_key: str):
        object.__setattr__(self, "_builder", builder)
        object.__setattr__(self, "_axis_key", axis_key)

    def __setattr__(self, name: str, value):
        if name.startswith("_"):
            object.__setattr__(self, name, value)
            return
        if isinstance(value, (_MeasurementValueProxy, _MeasurementReadProxy)):
            value = object.__getattribute__(value, "_value")
        value = _plain_json_value(value)
        axis_key = object.__getattribute__(self, "_axis_key")
        builder = object.__getattribute__(self, "_builder")
        _check_recordable(value, f"Aggregation '{name}' of '{builder._name}.{axis_key}'")
        aggs = builder._y_axis_aggregations.setdefault(axis_key, [])
        for agg in aggs:
            if agg["type"] == name:
                agg["value"] = value
                builder._flush()
                return
        aggs.append({"type": name, "value": value})
        builder._flush()

    def __getattr__(self, name: str):
        if name.startswith("_"):
            raise AttributeError(f"'_YAxisAggregationsWriteProxy' object has no attribute '{name}'")
        axis_key = object.__getattribute__(self, "_axis_key")
        builder = object.__getattribute__(self, "_builder")
        for agg in builder._y_axis_aggregations.get(axis_key, []):
            if agg["type"] == name:
                return agg.get("value")
        raise AttributeError(f"No aggregation '{name}' found on y-axis '{axis_key}'")


class _YAxisDataProxy:
    def __init__(self, data: list, builder: "_MDMBuilder", axis_key: str):
        object.__setattr__(self, "_data", data)
        object.__setattr__(self, "_builder", builder)
        object.__setattr__(self, "_axis_key", axis_key)

    @property
    def aggregations(self):
        return _YAxisAggregationsWriteProxy(
            object.__getattribute__(self, "_builder"),
            object.__getattribute__(self, "_axis_key"),
        )

    def __iter__(self):
        return iter(self._data)

    def __len__(self):
        return len(self._data)

    def __getitem__(self, idx):
        return self._data[idx]

    def __repr__(self):
        return repr(self._data)


class _YAxisProxy:
    def __init__(self, builder: "_MDMBuilder"):
        object.__setattr__(self, "_builder", builder)

    def __setattr__(self, key: str, data):
        if key.startswith("_"):
            object.__setattr__(self, key, data)
            return
        data = _plain_json_value(data)
        if not isinstance(data, list):
            raise ValueError(f"Y-axis data must be a list, got {type(data)}")
        _check_recordable(data, f"Y-axis '{key}' of '{self._builder._name}'")
        self._builder._y_axes[key] = data
        self._builder._flush()

    def __getattr__(self, key: str):
        if key.startswith("_"):
            raise AttributeError(f"'_YAxisProxy' object has no attribute '{key}'")
        if key in self._builder._y_axes:
            return _YAxisDataProxy(self._builder._y_axes[key], self._builder, key)
        raise AttributeError(f"No y-axis '{key}' set")


class _MDMBuilder:
    def __init__(self, name: str, parent: "Measurements"):
        object.__setattr__(self, "_name", name)
        object.__setattr__(self, "_parent", parent)
        object.__setattr__(self, "_x_axis_data", None)
        object.__setattr__(self, "_y_axes", {})
        object.__setattr__(self, "_y_axis_aggregations", {})
        object.__setattr__(self, "_y_axis_proxy", _YAxisProxy(self))

    @property
    def x_axis(self):
        return self._x_axis_data

    @x_axis.setter
    def x_axis(self, data):
        data = _plain_json_value(data)
        if not isinstance(data, list):
            raise ValueError(f"X-axis data must be a list, got {type(data)}")
        _check_recordable(data, f"X-axis of '{self._name}'")
        object.__setattr__(self, "_x_axis_data", data)
        self._flush()

    @property
    def y_axis(self):
        return self._y_axis_proxy

    def _flush(self):
        if self._x_axis_data is None or not self._y_axes:
            return
        y_axis_list = []
        for key, data in self._y_axes.items():
            axis_dict = {"key": key, "data": data}
            if key in self._y_axis_aggregations:
                axis_dict["aggregations"] = self._y_axis_aggregations[key]
            y_axis_list.append(axis_dict)
        multidim_value = {
            "x_axis": {"data": self._x_axis_data},
            "y_axis": y_axis_list,
        }

        from datetime import datetime, timezone

        self._parent._measurements = [
            m for m in self._parent._measurements if m["name"] != self._name
        ]
        self._parent._measurements.append({
            "name": self._name,
            "value": {"MultiDimensional": multidim_value},
            "unit": None,
            "timestamp": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        })


class _YAxisReadProxy:
    def __init__(self, y_axes: list):
        object.__setattr__(self, "_y_axes", y_axes)
        lookup = {}
        for axis in y_axes:
            if isinstance(axis, dict):
                key = axis.get("key")
                if key:
                    lookup[key] = axis.get("data")
        object.__setattr__(self, "_lookup", lookup)

    def __getattr__(self, key: str):
        if key.startswith("_"):
            raise AttributeError(f"'_YAxisReadProxy' object has no attribute '{key}'")
        if key in self._lookup:
            return self._lookup[key]
        raise AttributeError(f"No y-axis '{key}' found")


class _MDMReadProxy:
    def __init__(self, mdm_data: dict):
        object.__setattr__(self, "_data", mdm_data)

    @property
    def x_axis(self):
        x = self._data.get("x_axis", {})
        return x.get("data") if isinstance(x, dict) else x

    @property
    def y_axis(self):
        return _YAxisReadProxy(self._data.get("y_axis", []))


class _AggregationsWriteProxy:
    def __init__(self, measurement_dict: dict):
        object.__setattr__(self, "_measurement", measurement_dict)

    def __setattr__(self, name: str, value):
        if name.startswith("_"):
            object.__setattr__(self, name, value)
            return
        if isinstance(value, (_MeasurementValueProxy, _MeasurementReadProxy)):
            value = object.__getattribute__(value, "_value")
        value = _plain_json_value(value)
        measurement = object.__getattribute__(self, "_measurement")
        _check_recordable(value, f"Aggregation '{name}' of '{measurement.get('name')}'")
        if "aggregations" not in measurement:
            measurement["aggregations"] = []
        for agg in measurement["aggregations"]:
            if agg["type"] == name:
                agg["value"] = value
                return
        measurement["aggregations"].append({"type": name, "value": value})

    def __getattr__(self, name: str):
        if name.startswith("_"):
            raise AttributeError(f"'_AggregationsWriteProxy' object has no attribute '{name}'")
        measurement = object.__getattribute__(self, "_measurement")
        if "aggregations" in measurement:
            for agg in measurement["aggregations"]:
                if agg["type"] == name:
                    return agg.get("value")
        raise AttributeError(f"No aggregation '{name}' found")

    def __repr__(self):
        measurement = object.__getattribute__(self, "_measurement")
        types = [a["type"] for a in measurement.get("aggregations", [])]
        return f"<_AggregationsWriteProxy({types})>"


class _MeasurementValueProxy:
    """Wraps a measurement value + dict reference. Behaves as the raw value but exposes .aggregations."""

    def __init__(self, value, measurement_dict: dict):
        object.__setattr__(self, "_value", value)
        object.__setattr__(self, "_measurement", measurement_dict)

    @property
    def aggregations(self):
        return _AggregationsWriteProxy(object.__getattribute__(self, "_measurement"))

    def _unwrap(self, other):
        if isinstance(other, (_MeasurementValueProxy, _MeasurementReadProxy)):
            return object.__getattribute__(other, "_value")
        return other

    def __repr__(self):
        return repr(self._value)

    def __str__(self):
        return str(self._value)

    def __bool__(self):
        return bool(self._value)

    def __hash__(self):
        return hash(self._value)

    def __float__(self):
        return float(self._value)

    def __int__(self):
        return int(self._value)

    def __index__(self):
        return self._value.__index__()

    def __eq__(self, other):
        return self._value == self._unwrap(other)

    def __ne__(self, other):
        return self._value != self._unwrap(other)

    def __lt__(self, other):
        return self._value < self._unwrap(other)

    def __le__(self, other):
        return self._value <= self._unwrap(other)

    def __gt__(self, other):
        return self._value > self._unwrap(other)

    def __ge__(self, other):
        return self._value >= self._unwrap(other)

    def __add__(self, other):
        return self._value + self._unwrap(other)

    def __radd__(self, other):
        return other + self._value

    def __sub__(self, other):
        return self._value - self._unwrap(other)

    def __rsub__(self, other):
        return other - self._value

    def __mul__(self, other):
        return self._value * self._unwrap(other)

    def __rmul__(self, other):
        return other * self._value

    def __truediv__(self, other):
        return self._value / self._unwrap(other)

    def __rtruediv__(self, other):
        return other / self._value

    def __neg__(self):
        return -self._value

    def __abs__(self):
        return abs(self._value)

    def __round__(self, ndigits=None):
        return round(self._value, ndigits)

    def __len__(self):
        return len(self._value)

    def __iter__(self):
        return iter(self._value)

    def __getitem__(self, key):
        return self._value[key]

    def __contains__(self, item):
        return item in self._value


class Measurements:
    def __repr__(self):
        # What `--locals` shows: the values recorded so far, latest wins.
        latest = {}
        for m in object.__getattribute__(self, "_measurements"):
            value = m["value"]
            if isinstance(value, dict) and len(value) == 1:
                value = next(iter(value.values()))
            elif value == "Null":
                value = None
            latest[m["name"]] = value
        return "Measurements(" + ", ".join(f"{k}={v!r}" for k, v in latest.items()) + ")"

    def __init__(self, event_queue=None, job_id=None):
        object.__setattr__(self, "_measurements", [])
        object.__setattr__(self, "_mdm_builders", {})
        # Same motivation as Logs/LogCapturingStream: emit each recorded
        # measurement onto the orchestrator's live event stream so agents
        # see partial progress without waiting for phase end.
        object.__setattr__(self, "_event_queue", event_queue)
        object.__setattr__(self, "_job_id", job_id)

    # `measurements["ch_1"] = v`, as `inputs[...]` reads: a name built in
    # a loop over channels records like `measurements.ch_1 = v`.
    def __setitem__(self, name: str, value):
        if not isinstance(name, str) or name.startswith("_"):
            raise KeyError(f"measurement name must be a string not starting with `_`: {name!r}")
        self.__setattr__(name, value)

    def __getitem__(self, name: str):
        if not isinstance(name, str) or name.startswith("_"):
            raise KeyError(name)
        return self.__getattr__(name)

    def __contains__(self, name) -> bool:
        """`"ch_1" in measurements`: recorded in this phase."""
        recorded = object.__getattribute__(self, "_measurements")
        if any(m.get("name") == name for m in recorded):
            return True
        # A builder a mere read created holds nothing yet.
        builder = object.__getattribute__(self, "_mdm_builders").get(name)
        return builder is not None and (
            object.__getattribute__(builder, "_x_axis_data") is not None
            or bool(object.__getattribute__(builder, "_y_axes"))
        )

    # Without it, `in` and `for` fall back to `__getitem__(0)`.
    def __iter__(self):
        raise TypeError("measurements is not iterable: read or set one by name")

    def __setattr__(self, name: str, value):
        if name.startswith("_"):
            object.__setattr__(self, name, value)
            return

        if isinstance(value, (_MeasurementValueProxy, _MeasurementReadProxy)):
            value = object.__getattribute__(value, "_value")
        if not isinstance(value, MultiDim):
            value = _plain_json_value(value)

        if len(name.strip()) > _MEASUREMENT_NAME_MAX_CHARS:
            raise ValueError(
                f"Measurement name '{name[:40]}...' is {len(name)} characters; "
                f"the server accepts at most {_MEASUREMENT_NAME_MAX_CHARS}"
            )

        if name in self._mdm_builders:
            del self._mdm_builders[name]

        from datetime import datetime, timezone

        if isinstance(value, MultiDim):
            measurement_value = {"MultiDimensional": value.to_dict()}
        elif isinstance(value, bool):
            measurement_value = {"Boolean": value}
        elif isinstance(value, (int, float)):
            measurement_value = {"Numeric": float(value)}
        elif isinstance(value, str):
            measurement_value = {"String": value}
        elif isinstance(value, list):
            measurement_value = {"Array": value}
        elif isinstance(value, dict):
            measurement_value = {"Object": value}
        elif value is None:
            measurement_value = "Null"
        else:
            try:
                measurement_value = {"Numeric": float(value)}
            except (TypeError, ValueError):
                raise TypeError(
                    f"Measurement '{name}' cannot be recorded: {type(value).__name__} "
                    f"{_short_repr(value)} is not a number. Record a number, string, "
                    "bool, list or dict"
                ) from None

        # A number as written in the message (`nan`), not as wrapped for
        # the wire (`{'Numeric': nan}`).
        number = measurement_value.get("Numeric") if isinstance(measurement_value, dict) else None
        _check_recordable(number if number is not None else measurement_value, f"Measurement '{name}'")

        measurement = {
            "name": name,
            "value": measurement_value,
            "unit": None,
            "timestamp": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        }

        # Set twice in a phase, the last value is the measurement: an
        # earlier one left in the list was judged and uploaded as well.
        recorded = object.__getattribute__(self, "_measurements")
        recorded[:] = [m for m in recorded if m.get("name") != name]
        recorded.append(measurement)

        if self._event_queue is not None:
            try:
                self._event_queue.put({
                    "type": "MeasurementRecorded",
                    "job_id": self._job_id,
                    "name": name,
                    "value": measurement_value,
                    "unit": None,
                    "timestamp": measurement["timestamp"],
                })
            except Exception:
                pass

    def __getattr__(self, name: str):
        if name.startswith("_"):
            raise AttributeError(f"'Measurements' object has no attribute '{name}'")

        if name in self._mdm_builders:
            return self._mdm_builders[name]

        for measurement in reversed(self._measurements):
            if measurement["name"] == name:
                value_wrapper = measurement["value"]
                if isinstance(value_wrapper, dict):
                    if "Numeric" in value_wrapper:
                        return _MeasurementValueProxy(value_wrapper["Numeric"], measurement)
                    elif "String" in value_wrapper:
                        return _MeasurementValueProxy(value_wrapper["String"], measurement)
                    elif "Boolean" in value_wrapper:
                        return _MeasurementValueProxy(value_wrapper["Boolean"], measurement)
                    elif "Array" in value_wrapper:
                        return _MeasurementValueProxy(value_wrapper["Array"], measurement)
                    elif "Object" in value_wrapper:
                        return _MeasurementValueProxy(value_wrapper["Object"], measurement)
                    elif "MultiDimensional" in value_wrapper:
                        return _MDMReadProxy(value_wrapper["MultiDimensional"])
                elif value_wrapper == "Null":
                    return _MeasurementValueProxy(None, measurement)
                return _MeasurementValueProxy(value_wrapper, measurement)

        builder = _MDMBuilder(name, self)
        self._mdm_builders[name] = builder
        return builder

    def to_list(self):
        return self._measurements


# Connect keeps a fixed timeout: the plug service is a local process
# whose kernel backlog completes the handshake even mid-call, so a
# connect that hangs means the process is frozen (typically an EDR
# suspending the interpreter), not busy. Sized to match
# JOB_ACK_TIMEOUT_SECS (constants.rs), the engine's deadline for the
# same alive-but-not-executing failure. I/O has no timeout of its own —
# a plug method legitimately runs as long as the phase allows (thermal
# soak, firmware flash), so its only deadline is the phase's.
PLUG_CONNECT_TIMEOUT_S = 30.0

# A connect() that is refused or reset is retried: the request was never
# delivered, so replaying it cannot double-execute an instrument write.
# Nothing after a successful connect is ever retried — once the request
# line may have reached the service, only the caller knows whether the
# method is idempotent. Covers the window where the engine respawns a
# plug service. Connection bursts are handled server-side (threaded
# accept in tp_plug.py); this retry is not the fix for them.
PLUG_CONNECT_ATTEMPTS = 3
PLUG_CONNECT_RETRY_BASE_S = 0.05


# What a phase can pass when it calls a plug method. The call crosses a
# process boundary as `CallMethod{args_json}`, a JSON LIST (see
# `Plug.call`); this proxy sends no `kwargs_json`, although the plug
# service would accept one (the Studio debugger sends it). So this is
# the phase call's convention, not a wire limit, and `Plug.__getattr__`
# refuses keywords rather than silently dropping them. The introspection
# that Studio shows next to each method (`_describe_plug_methods`) reads
# the SAME flag, so a signature is reported the way a phase may actually
# call it. Change the convention, flip this, and both follow.
PLUG_CALL_ACCEPTS_KEYWORDS = False


class PlugError(Exception):
    """A plug method raised, with an exception class this process cannot
    rebuild (one only the plug's own process imported)."""


class _RemoteTraceback(Exception):
    """The plug process's traceback, chained under the exception a phase
    sees (`concurrent.futures` does the same): the frames it ran in."""

    def __init__(self, tb: str):
        self.tb = tb

    def __str__(self):
        return "\n" + self.tb.rstrip()


def _plug_exception(response: Dict[str, Any], label: str) -> BaseException:
    """The exception a failed plug call raises in the phase: the method's
    own class when this process already has it (a builtin, a module the
    phase imported), else `PlugError`. Never imports a module for it: a
    driver module can open an instrument at import."""
    message = response.get("error_message")
    module = response.get("error_module")
    qualname = response.get("error_qualname")
    cls = None
    if message is not None and module in sys.modules and qualname:
        found = sys.modules[module]
        for part in qualname.split("."):
            found = getattr(found, part, None)
        if isinstance(found, type) and issubclass(found, Exception):
            cls = found
    if cls is not None:
        try:
            # Built without its `__init__`: its signature is the plug
            # author's, the message is all that travelled.
            exc = cls.__new__(cls)
            exc.args = (f"{label}: {message}",)
            str(exc)
            return exc
        except Exception:
            pass
    name = qualname or "error"
    text = f"{name}: {message}" if message is not None else (
        response.get("error") or "plug call failed"
    )
    return PlugError(f"{label}: {text}")


class Plug:
    """NDJSON TCP client for communicating with hardware plug services"""

    # Its own public names are `call` and `deadline` (plugs.mdx says so);
    # the rest are `_tp_*`, so a driver's `name()` or `address` reaches
    # the driver.

    def __repr__(self):
        # What a phase's variables show (`--locals`): the plug it stands
        # for, not this proxy's address in memory.
        return f"<plug {self._tp_name}>"

    def __init__(self, name: str, address: str):
        self._tp_name = name
        self._tp_address = address
        # time.monotonic() value at which the owning phase times out, or
        # None for no phase timeout. Stamped by execute_job_streaming
        # before the phase thread starts. Monotonic, not wall-clock: an
        # NTP step during an hours-long phase must not eat (or extend)
        # its budget.
        self.deadline = None

    def _remaining(self, method: str) -> Optional[float]:
        """Seconds left before the phase deadline, None if unlimited."""
        if self.deadline is None:
            return None
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError(
                f"phase timed out before {method}() completed"
            )
        return remaining

    def __getattr__(self, method_name: str):
        # Probes (`copy`, `pickle`) and our own names never reach the plug.
        if method_name.startswith("__") or method_name.startswith("_tp_"):
            raise AttributeError(method_name)

        def method_wrapper(*args, **kwargs):
            if kwargs and not PLUG_CALL_ACCEPTS_KEYWORDS:
                raise TypeError(
                    f"{method_name}() does not support keyword arguments. Use positional arguments only."
                )
            return self.call(method_name, list(args), kwargs or None)

        return method_wrapper

    def _connect(
        self, host: str, port: int, method: str, remaining: Optional[float]
    ) -> socket.socket:
        """Open the per-call connection, retrying refused/reset connects."""
        last_error: Optional[BaseException] = None
        for attempt in range(1, PLUG_CONNECT_ATTEMPTS + 1):
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            try:
                sock.settimeout(
                    PLUG_CONNECT_TIMEOUT_S
                    if remaining is None
                    else min(PLUG_CONNECT_TIMEOUT_S, remaining)
                )
                sock.connect((host, port))
                return sock
            except (ConnectionRefusedError, ConnectionResetError) as e:
                sock.close()
                last_error = e
                if attempt == PLUG_CONNECT_ATTEMPTS:
                    break
                delay = PLUG_CONNECT_RETRY_BASE_S * attempt + random.uniform(
                    0, PLUG_CONNECT_RETRY_BASE_S
                )
                # Never sleep past the phase deadline.
                remaining = self._remaining(method)
                if remaining is not None and delay >= remaining:
                    break
                time.sleep(delay)
            except BaseException:
                sock.close()
                raise
        raise last_error

    def call(self, method: str, args: list = None, kwargs: dict = None) -> Any:
        """Call a method on the plug service via NDJSON TCP. `kwargs` only
        ever arrives when PLUG_CALL_ACCEPTS_KEYWORDS is on: the service
        accepts `kwargs_json` (the Studio debugger sends it), so flipping
        the flag forwards keywords instead of silently dropping them."""
        if args is None:
            args = []

        try:
            request = {
                "type": "CallMethod",
                "method": method,
                "args_json": json.dumps(args) if args else "",
            }
            if kwargs:
                request["kwargs_json"] = json.dumps(kwargs)

            remaining = self._remaining(method)

            host, port_str = self._tp_address.split(":")
            sock = self._connect(host, int(port_str), method, remaining)
            try:
                sock.settimeout(self._remaining(method))
                sock.sendall((json.dumps(request) + "\n").encode("utf-8"))

                response_data = b""
                while True:
                    # Re-derive the deadline per chunk so a slow-drip
                    # response cannot stretch past the phase timeout.
                    sock.settimeout(self._remaining(method))
                    try:
                        chunk = sock.recv(65536)
                    except socket.timeout:
                        raise TimeoutError("phase timed out") from None
                    if not chunk:
                        break
                    response_data += chunk
                    if b"\n" in response_data:
                        break

                line = response_data.split(b"\n", 1)[0]
                response = json.loads(line.decode("utf-8"))
            finally:
                sock.close()

        except TimeoutError as e:
            # Kept a TimeoutError: a phase can catch the deadline apart
            # from a failure of the instrument.
            raise TimeoutError(f"{self._tp_name}.{method}(): {e}") from None
        except Exception as e:
            raise PlugError(f"{self._tp_name}.{method}(): {e}") from None

        if response.get("success"):
            result_json = response.get("result_json")
            return json.loads(result_json) if result_json else None
        remote = response.get("error") or ""
        # Chained only when the plug's side has frames to show: an
        # exception line alone (a call refused before it ran) is already
        # the message of the exception raised here.
        raise _plug_exception(response, f"{self._tp_name}.{method}()") from (
            _RemoteTraceback(remote) if remote.startswith("Traceback") else None
        )


# ============================================================================
# Main worker functions
# ============================================================================

def _dotted_name(module_path: str, procedure_dir: Optional[Path]):
    """`steps.impl` for `<procedure>/steps/impl.py`, `steps` for
    `<procedure>/steps/__init__.py`: the name a file of the procedure tree
    imports under, so its relative imports (`from .base import X`) find
    their package. `None` outside the tree or at its root."""
    if procedure_dir is None:
        return None
    try:
        rel = Path(module_path).resolve().relative_to(Path(procedure_dir).resolve())
    except ValueError:
        return None
    parts = list(rel.with_suffix("").parts)
    if parts and parts[-1] == "__init__":
        parts.pop()
    if len(parts) < 2 and not rel.name == "__init__.py":
        return None
    if not parts or not all(p.isidentifier() for p in parts):
        return None
    return ".".join(parts)


# Package modules of the procedure, by dotted name: the file's
# (mtime, size) when it was last (re)loaded.
_PACKAGE_MODULE_STAMPS: Dict[str, tuple] = {}


def _in_regular_package(module_path: str, procedure_dir: Path) -> bool:
    """Every folder from the file's up to the procedure root holds an
    `__init__.py`: a package the author wrote as one. Only there is a
    module imported once and kept (one copy per class); a folder of phase
    files (`phases/`) stays run fresh on every job."""
    root = Path(procedure_dir).resolve()
    folder = Path(module_path).resolve().parent
    while folder != root:
        if not (folder / "__init__.py").is_file() or folder == folder.parent:
            return False
        folder = folder.parent
    return True


def _import_package_module(dotted: str, module_path: str, procedure_dir: Path):
    """A module of a package of the procedure, imported as Python does
    (parents first, one copy in `sys.modules`, so its classes are the
    ones its siblings import), the whole package imported afresh when one
    of its files changed: an edited phase or helper runs as saved. `None`
    when the name is another module's (an installed or stdlib package of
    the same name)."""
    root_dir = os.path.realpath(procedure_dir) + os.sep
    root = dotted.split(".", 1)[0]

    def ours(name):
        module = sys.modules.get(name)
        where = getattr(module, "__file__", None) or next(iter(getattr(module, "__path__", []) or []), "")
        return os.path.realpath(where or "").startswith(root_dir)

    # The top name already belongs to a module outside the procedure:
    # never purged, and the file is loaded on its own.
    if root in sys.modules and not ours(root):
        return None

    def family():
        return [
            n for n in list(sys.modules) if (n == root or n.startswith(root + ".")) and ours(n)
        ]

    def stamp_of(name):
        path = getattr(sys.modules.get(name), "__file__", None)
        try:
            st = os.stat(path)
            return (st.st_mtime_ns, st.st_size)
        except (TypeError, OSError):
            return None

    try:
        # Any file of the package edited since it was loaded (the phase's,
        # a helper's, the `__init__` re-exporting it): the whole package is
        # imported afresh, so the edit runs and no stale copy lingers.
        changed = [n for n in family() if _PACKAGE_MODULE_STAMPS.get(n) != stamp_of(n)]
        if changed:
            for n in changed:
                # Bytecode is validated by the source's whole-second mtime
                # and its size: an edit within the second of the last
                # import, same length, would run the old code.
                path = getattr(sys.modules.get(n), "__file__", None)
                try:
                    os.remove(importlib.util.cache_from_source(path))
                except (TypeError, ValueError, NotImplementedError, OSError):
                    pass
            for n in family():
                del sys.modules[n]
                _PACKAGE_MODULE_STAMPS.pop(n, None)
            importlib.invalidate_caches()
        module = importlib.import_module(dotted)
        if os.path.realpath(getattr(module, "__file__", "") or "") != os.path.realpath(module_path):
            return None
        for n in family():
            _PACKAGE_MODULE_STAMPS[n] = stamp_of(n)
        return module
    except ImportError as e:
        # The module's own import failed (it raised, or names a missing
        # dependency): that error is the phase's to report.
        if getattr(e, "name", None) not in (dotted, dotted.rsplit(".", 1)[0]):
            raise
        return None


def load_module(module_path: str, procedure_dir: Optional[Path] = None):
    """Load a Python module from file, fresh on every call (an edited
    phase file runs as saved), under its dotted name inside the procedure
    tree so its relative imports resolve. A module of a regular package
    (`__init__.py` up to the root) is imported instead, its classes
    existing once (`_import_package_module`)."""
    dotted = _dotted_name(module_path, procedure_dir)
    if dotted and _in_regular_package(module_path, procedure_dir):
        module = _import_package_module(dotted, module_path, procedure_dir)
        if module is not None:
            return module
    is_package = Path(module_path).name == "__init__.py"
    spec = importlib.util.spec_from_file_location(
        dotted or "phase_module",
        module_path,
        submodule_search_locations=[str(Path(module_path).parent)] if is_package else None,
    )
    if spec is None or spec.loader is None:
        raise ImportError(f"Cannot load module from {module_path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def load_phase_module(module_name: str, procedure_dir: Path):
    """Resolve a phase's `python:` module reference and import it.

    Shared by job execution and --introspect: introspection must resolve
    modules exactly the way execution does, or it reports signatures for
    the wrong file.
    """
    is_file_path = "/" in module_name or "\\" in module_name

    if is_file_path:
        if module_name.startswith("~"):
            module_file = Path(module_name).expanduser()
        else:
            module_file = procedure_dir / module_name
        if module_file.is_dir():
            module_file = module_file / "__init__.py"
        elif not module_file.suffix == ".py":
            module_file = module_file.with_suffix(".py")
    else:
        leading_dots = len(module_name) - len(module_name.lstrip("."))

        if leading_dots > 0:
            parent_path = "../" * (leading_dots - 1)
            remaining_path = module_name[leading_dots:].replace(".", "/")
            module_path = "phases/" + parent_path + remaining_path + ".py"
        else:
            module_path = module_name.replace(".", "/") + ".py"

        module_file = procedure_dir / module_path
        # A package (`python: steps:fn` for `steps/__init__.py`).
        package_init = module_file.with_suffix("") / "__init__.py"
        if not module_file.exists() and package_init.exists():
            module_file = package_init

    try:
        module_file = module_file.resolve()
    except:
        pass

    # Two-stage resolution: file-path first (in-tree phases /
    # plugs of the procedure), then standard `importlib` if the
    # file isn't on disk. The importlib path lets
    # `python: shared.foo:Bar` resolve to a workspace-installed
    # wheel (uv-workspace monorepo layout) without requiring
    # relative dot-prefixed module names. Skip the fallback for
    # explicit file-path modules ("/", "\\" in name) and for
    # leading-dot or "~" prefixes — those mean "look in this
    # tree", not "look in site-packages".
    #
    # Threat model note: this DOES technically allow phase YAML
    # to call arbitrary importable modules (e.g.
    # `python: subprocess:Popen`). The procedure is already
    # trusted to ship and run Python code, so the same author
    # could just write `import subprocess; subprocess.Popen(...)`
    # inside a phase file. We're not adding capability — only
    # changing the spelling. Hosts that need stricter isolation
    # should sandbox at the venv / OS layer, not at the YAML.
    if module_file.exists():
        return load_module(str(module_file), procedure_dir)
    elif (
        not is_file_path
        and not module_name.startswith(".")
        and not module_name.startswith("~")
    ):
        import importlib as _importlib
        try:
            return _importlib.import_module(module_name)
        except ModuleNotFoundError as exc:
            raise FileNotFoundError(
                f"Module {module_name} not found in project or venv: {exc}"
            ) from None
    else:
        raise FileNotFoundError(f"Module {module_name} not found at {module_file}")


def _raise_site(e: BaseException):
    """`(file, line)` of the innermost frame of `e` outside this worker:
    where the phase's own code raised, for the log line that reports it.
    `(None, None)` when every frame is the worker's."""
    here = os.path.abspath(__file__)
    site = (None, None)
    tb = e.__traceback__
    while tb is not None:
        filename = tb.tb_frame.f_code.co_filename
        if os.path.abspath(filename) != here:
            site = (_relativize_log_path(filename), tb.tb_lineno)
        tb = tb.tb_next
    return site


def _phase_traceback(e: BaseException) -> str:
    """The traceback of a phase's exception without this worker's own
    frames (the call into the phase, its source line, the plug proxy):
    they are the same on every failure and point at nothing the author
    can fix. Chained causes (`raise ... from`, an error while handling
    another) are kept, filtered the same way."""
    here = os.path.abspath(__file__)
    te = traceback.TracebackException.from_exception(e)
    node, seen = te, set()
    while node is not None and id(node) not in seen:
        seen.add(id(node))
        kept = [f for f in node.stack if os.path.abspath(f.filename) != here]
        if kept:
            node.stack = traceback.StackSummary.from_list(kept)
        node = node.__cause__ or (
            None if node.__suppress_context__ else node.__context__
        )
    return "".join(te.format())


def execute_job_streaming(
    command: Dict[str, Any], procedure_dir: Path, engine: Optional[_EngineChannel] = None
):
    """Execute a single job with streaming events via generator. `engine`
    carries the engine's replies on the job connection (`ui.wait()`)."""
    job_id = command["job_id"]
    slot_id = command["slot_id"]
    phase_name = command["phase_name"]
    module_name = command.get("module", command.get("file", ""))
    function_name = command["function"]
    plugs = command.get("plugs", {})
    # None = no phase timeout: the protocol always sends the key,
    # null when the procedure YAML has no `timeout` for this phase.
    timeout_ms = command.get("timeout_ms")
    retry_count = command.get("retry_count", 0)
    retry_limit = command.get("retry_limit", 0)
    phase_results = command.get("phase_results", {})
    # A phase of a sequence: the call's resolved inputs, and the plug
    # parameter names the sequence spells that the procedure bound to
    # another plug key.
    sequence_inputs = Inputs(command.get("inputs") or {})
    plug_aliases = command.get("plug_aliases") or {}
    # Plugs of the run that did not open: a parameter naming one makes the
    # phase SKIP, it never ran.
    unavailable_plugs = {k.lower(): v for k, v in (command.get("unavailable_plugs") or {}).items()}
    # Resolved by the engine when the run started: the phase's `with:`
    # (keyword arguments), its `env:`, and the config and secrets it reads.
    with_args = dict(command.get("with") or {})
    phase_env = dict(command.get("env") or {})
    run_config = Config(command.get("config") or {})
    run_secrets = Secrets(command.get("secrets") or {})

    run = Run(slot_id, job_id, retry_count, retry_limit, command.get("run_context"))
    # Measurements, Logs, UI, Attachments — each wants the live event_queue
    # so partial results reach the orchestrator before phase end (crucial
    # for long-running phases and force-kill diagnostics). Deferred until
    # after event_queue is constructed below.
    unit_info = command.get("unit_info") or {}
    unit = Unit(initial_metadata=unit_info.get("metadata"))
    # Snapshot the seeded (operator/identify) metadata: result dicts ship
    # only the DELTA this phase wrote. Re-shipping the full seed from
    # every phase would let an untouched later phase revert an earlier
    # phase's override in the CLI's last-write-wins merge.
    initial_unit_metadata = dict(unit.metadata)

    def _unit_metadata_delta():
        delta = {
            k: v
            for k, v in unit.metadata.items()
            if initial_unit_metadata.get(k) != v
        }
        return delta or None

    def _run_metadata_bundle():
        return dict(run.metadata) if run.metadata else None

    if unit_info:
        unit.serial_number = unit_info.get("serial_number")
        unit.part_number = unit_info.get("part_number")
        unit.revision_number = unit_info.get("revision_number")
        unit.batch_number = unit_info.get("batch_number")
        # Run attribution rides the same command payload but surfaces
        # on the run object — it is a run property, not a unit field.
        run.operated_by = unit_info.get("operated_by")
        sub_units = unit_info.get("sub_units", {})
        if sub_units:
            unit.sub_units = SubUnitsDict(sub_units)

    event_queue = EventQueue()
    measurements = Measurements(event_queue=event_queue, job_id=job_id)
    logs = Logs(job_id=job_id, event_queue=event_queue)
    ui = UIContext(
        job_id,
        event_queue,
        command.get("ui_components"),
        engine,
        panel=bool(command.get("ui_panel")),
        events=command.get("ui_events"),
    )
    attachments = Attachments(job_id, event_queue)

    plugs_dict = {}
    for plug_name, plug_address in plugs.items():
        plugs_dict[plug_name] = Plug(plug_name, plug_address)

    # The phase's `env:` holds while it runs (module import included) and
    # is undone after, so the next phase on this worker starts from the
    # procedure's environment.
    saved_env = _apply_env(phase_env)

    original_stdout_stream = sys.stdout
    original_stderr_stream = sys.stderr

    sys.stdout = LogCapturingStream(logs, level="INFO", job_id=job_id, event_queue=event_queue)
    sys.stderr = LogCapturingStream(logs, level="WARNING", job_id=job_id, event_queue=event_queue)

    # Bridge stdlib logging into this job's log pipeline (same per-job,
    # process-global pattern as the stdout/stderr swap above). The root
    # logger's default WARNING threshold would hide INFO records before
    # the bridge sees them, so lower it for the duration of the job;
    # loggers with an explicit level still filter as configured. DEBUG
    # records are deliberately not displayed in phase logs.
    logging_bridge = StdlibLoggingBridge(logs)
    logging_bridge.setLevel(logging.INFO)
    root_logger = logging.getLogger()
    previous_root_level = root_logger.level
    root_logger.addHandler(logging_bridge)
    if root_logger.getEffectiveLevel() > logging.INFO:
        root_logger.setLevel(logging.INFO)

    phase_result_container = {"error": None, "result": None}

    def run_phase():
        # Phases run on this spawned thread, not the main thread, so the
        # debugger must be told to trace it explicitly.
        _debug_this_thread()
        try:
            module = load_phase_module(module_name, procedure_dir)

            if not hasattr(module, function_name):
                raise AttributeError(f"Function `{function_name}` not found in {module_name}")

            func = getattr(module, function_name)

            # Module-level code is done. Tell the Rust side so it can
            # drop the module-import deadline — from here on the phase
            # body owns the clock (phase timeout, if configured).
            event_queue.put({"type": "ModuleLoaded", "job_id": job_id})

            sig = inspect.signature(func)
            kwargs = {}
            phase_instance = Phase()
            plug_keys_lower = {k.lower() for k in plugs_dict}

            def plug_named(name):
                return (
                    name in plug_keys_lower
                    or plug_aliases.get(name, name).lower() in plug_keys_lower
                )

            plug_named_inputs = plug_named("inputs")
            plug_named_config = plug_named("config")
            plug_named_secrets = plug_named("secrets")

            not_run: Optional[str] = None
            for param_name, param in sig.parameters.items():
                if param.kind is inspect.Parameter.VAR_POSITIONAL:
                    continue
                if param.kind is inspect.Parameter.VAR_KEYWORD:
                    # `**kwargs` takes every `with:` key no named parameter
                    # took; the loader refuses a key nothing takes.
                    kwargs.update(
                        {k: v for k, v in with_args.items() if k not in sig.parameters}
                    )
                    continue
                if param_name in with_args:
                    # The loader refuses a `with:` key that names an
                    # injectable, a plug or a phase: nothing else claims it.
                    kwargs[param_name] = with_args[param_name]
                elif param_name == "phase":
                    kwargs[param_name] = phase_instance
                elif param_name == "run":
                    kwargs[param_name] = run
                elif param_name == "measurements":
                    kwargs[param_name] = measurements
                elif param_name == "log":
                    kwargs[param_name] = logs
                elif param_name == "ui":
                    kwargs[param_name] = ui
                elif param_name == "unit":
                    kwargs[param_name] = unit
                elif param_name == "attach":
                    kwargs[param_name] = attachments
                elif param_name == "dashboard":
                    kwargs[param_name] = dashboard_client()
                elif (
                    param_name == "inputs"
                    and not plug_named_inputs
                    and param_name not in phase_results
                ):
                    # A procedure with a plug or a phase keyed `inputs`
                    # predates this injectable: they keep the name.
                    kwargs[param_name] = sequence_inputs
                elif (
                    param_name == "config"
                    and not plug_named_config
                    and param_name not in phase_results
                ):
                    # Same guard as `inputs`: a plug or a phase keyed `config`
                    # predates this injectable and keeps the name.
                    kwargs[param_name] = run_config
                elif (
                    param_name == "secrets"
                    and not plug_named_secrets
                    and param_name not in phase_results
                ):
                    kwargs[param_name] = run_secrets
                elif param_name in phase_results:
                    kwargs[param_name] = PhaseResults(
                        param_name, phase_results[param_name]
                    )
                else:
                    # The plug key this parameter stands for: itself, or the
                    # procedure plug the sequence call bound it to.
                    wanted = plug_aliases.get(param_name, param_name).lower()
                    matched_plug = None
                    for plug_key, plug_instance in plugs_dict.items():
                        if wanted == plug_key.lower():
                            matched_plug = plug_instance
                            break
                    if matched_plug is not None:
                        kwargs[param_name] = matched_plug
                    elif (
                        wanted in unavailable_plugs
                        and sig.parameters[param_name].default is inspect.Parameter.empty
                    ):
                        # A default (`dmm=None`) means the phase copes
                        # without it: a teardown must still run then.
                        not_run = f"Not run: plug {wanted} did not open: {unavailable_plugs[wanted]}"
                        break
                    elif sig.parameters[param_name].default is inspect.Parameter.empty:
                        available = list(phase_results.keys()) if phase_results else []
                        available_plugs = sorted(plugs_dict.keys())
                        if param_name == "vars":
                            raise _BindingError(
                                f"Phase {function_name}: required parameter 'vars' "
                                f"unresolved: `vars` is now `config`; rename the parameter"
                            )
                        # `required parameter '<name>'` is matched by the engine
                        # (`explain_unavailable_plugs`) to say why a plug is missing.
                        raise _BindingError(
                            f"Phase {function_name}: required parameter '{param_name}' "
                            f"unresolved: no plug key or phase result {param_name} "
                            f"(plugs: {', '.join(available_plugs) or 'none'}; "
                            f"results: {', '.join(available) or 'none'}); add a plug with "
                            f"`key: {param_name}` or add `depends_on: [{param_name}]`"
                        )

            phase_result: Optional[PhaseResultType] = None
            error_message: Optional[str] = None

            try:
                if not_run is not None:
                    raise _PhaseResultException(PhaseResult.SKIP, not_run)
                return_value = func(**kwargs)
                # An `async def` phase: its coroutine runs to completion
                # here, on a loop of its own.
                if inspect.iscoroutine(return_value):
                    import asyncio

                    return_value = asyncio.run(return_value)

                if return_value is None:
                    phase_result = PhaseResult.CONTINUE
                else:
                    raise ValueError(
                        f"Phase {function_name} returned {return_value!r}; return nothing "
                        f"and use `phase.fail()`, `skip()`, `retry()` or `stop()`"
                    )
            except _PhaseResultException as e:
                phase_result = e.result_value
                error_message = e.message if e.message else None

                if error_message:
                    logs.info(error_message)

            unit_info = None
            if (
                unit.serial_number
                or unit.batch_number
                or unit.part_number
                or unit.revision_number
                or run.operated_by
                or unit.sub_units
            ):
                unit_info = {
                    "serial_number": unit.serial_number,
                    "batch_number": unit.batch_number,
                    "part_number": unit.part_number,
                    "revision_number": unit.revision_number,
                    # Run attribution, carried on the same result payload
                    # (see UnitInfo.operated_by in unit.rs).
                    "operated_by": run.operated_by,
                    "sub_units": dict(unit.sub_units) if unit.sub_units else None,
                    "status": "tested",
                }

            result_dict = {
                "success": True,
                "phase_result": phase_result,
                "measurements": measurements.to_list(),
                "logs": logs.entries,
                "unit": unit_info,
                "run_metadata": _run_metadata_bundle(),
                "unit_metadata": _unit_metadata_delta(),
            }

            phase_result_container["result"] = result_dict

        except SystemExit as e:
            exit_code = e.code if isinstance(e.code, int) else 1
            error_msg = f"Phase called sys.exit({exit_code})"
            print(error_msg, file=sys.stderr)

            if "logs" in locals():
                file, line = _raise_site(e)
                logs._add_log("ERROR", error_msg, file=file, line=line)

            phase_result_container["result"] = {
                "success": False,
                "error": error_msg,
                "exit_code": exit_code,
                "phase_result": PhaseResult.FAIL,
                "measurements": (
                    measurements.to_list() if "measurements" in locals() else []
                ),
                "logs": logs.entries if "logs" in locals() else [],
                # Metadata set before the crash still ships (like
                # measurements/logs) — a failed run is where diagnostic
                # metadata matters most.
                "run_metadata": _run_metadata_bundle(),
                "unit_metadata": _unit_metadata_delta(),
            }

        except UiWaitCancelled as e:
            # Not a bug in the phase: the run moved on under a blocked
            # `ui.wait()`. Report the reason, like a cancelled prompt.
            error_msg = str(e) or "ui.wait() was cancelled"
            if "logs" in locals():
                logs.error(f"ui.wait() cancelled: {error_msg}")
            phase_result_container["result"] = {
                "success": False,
                "error": error_msg,
                "phase_result": PhaseResult.FAIL,
                "measurements": (
                    measurements.to_list() if "measurements" in locals() else []
                ),
                "logs": logs.entries if "logs" in locals() else [],
                "run_metadata": _run_metadata_bundle(),
                "unit_metadata": _unit_metadata_delta(),
            }

        except Exception as e:
            # A parameter the procedure cannot fill is its mistake, not the
            # phase's code: the message alone, no worker frames.
            error_msg = str(e) if isinstance(e, _BindingError) else _phase_traceback(e)

            # The traceback is the phase's log line (the console prints it
            # there, once) and its error (what an agent and the run record
            # read).
            if "logs" in locals():
                file, line = _raise_site(e)
                logs._add_log("ERROR", error_msg, file=file, line=line)

            phase_result_container["error"] = error_msg
            phase_result_container["result"] = {
                "success": False,
                "error": error_msg,
                # Opt-in (`tofupilot run --locals`): `repr()` on a driver
                # object can talk to the instrument, and a station has no
                # reader for them.
                "locals": _failure_locals(e) if command.get("capture_locals") else [],
                "phase_result": PhaseResult.FAIL,
                "measurements": (
                    measurements.to_list() if "measurements" in locals() else []
                ),
                "logs": logs.entries if "logs" in locals() else [],
                "run_metadata": _run_metadata_bundle(),
                "unit_metadata": _unit_metadata_delta(),
            }

    try:
        # Disable the phase timeout only when debugpy is actually
        # listening (not merely when TP_DEBUG is set) so a breakpoint
        # pause isn't killed. Keying off _debugpy_started keeps this in
        # lockstep with the Rust timeout-skip: a worker without a live
        # debugger (import failed, or a crash-respawn without TP_DEBUG)
        # keeps its timeout enforced on both sides.
        timeout_seconds = None if _debugpy_started else (
            timeout_ms / 1000.0 if timeout_ms else None
        )

        # Monotonic: the timeout is a duration, and a wall-clock step
        # (NTP sync mid-burn-in) would otherwise cut the phase short or
        # let it overrun.
        start_time = time.monotonic()

        # Plug RPC calls share the phase's deadline instead of carrying
        # their own: a plug call must be allowed to run exactly as long
        # as the phase itself is. Stamped before the thread starts so
        # even a first-line plug call sees it.
        deadline = (start_time + timeout_seconds) if timeout_seconds else None
        for plug_instance in plugs_dict.values():
            plug_instance.deadline = deadline

        thread = threading.Thread(target=run_phase, daemon=False)
        thread.start()
        while thread.is_alive():
            if timeout_seconds and (time.monotonic() - start_time) > timeout_seconds:
                event_queue.close()
                # The phase thread cannot be killed: release a wait it is
                # blocked in, and drop an answer the engine sends after this.
                if engine is not None:
                    engine.close(f"Timed out after {_format_ms(timeout_ms)}")
                yield {
                    "type": "result",
                    "data": {
                        "success": False,
                        "error": f"Timed out after {_format_ms(timeout_ms)}",
                        "phase_result": PhaseResult.FAIL,
                        "measurements": measurements.to_list(),
                        "logs": logs.entries,
                        "run_metadata": _run_metadata_bundle(),
                        "unit_metadata": _unit_metadata_delta(),
                    }
                }
                return

            event = event_queue.get(timeout=0.1)
            if event is not None:
                yield {"type": "event", "data": event}

        thread.join(timeout=1.0)

        while not event_queue.empty():
            event = event_queue.get(timeout=0.1)
            if event is not None:
                yield {"type": "event", "data": event}

        # Yield the result dict when the phase produced one — even on the
        # generic-exception path, where it carries the traceback plus the
        # measurements/logs/metadata written before the crash. Raising
        # first (the old order) sent a bare Error message and silently
        # dropped all of that.
        if phase_result_container["result"]:
            yield {"type": "result", "data": phase_result_container["result"]}
        elif phase_result_container["error"]:
            raise Exception(phase_result_container["error"])
        else:
            yield {
                "type": "result",
                "data": {
                    "success": False,
                    "error": "Phase execution failed to produce result",
                    "phase_result": PhaseResult.FAIL,
                    "measurements": measurements.to_list(),
                    "logs": logs.entries,
                    "run_metadata": _run_metadata_bundle(),
                    "unit_metadata": _unit_metadata_delta(),
                }
            }

    finally:
        event_queue.close()
        sys.stdout = original_stdout_stream
        sys.stderr = original_stderr_stream
        root_logger.removeHandler(logging_bridge)
        root_logger.setLevel(previous_root_level)
        _restore_env(saved_env)


def _apply_env(env: Dict[str, str]) -> Dict[str, Optional[str]]:
    """Set `env` in `os.environ`; returns what it replaced, for
    `_restore_env` (None for a variable that was not set)."""
    saved = {}
    for key, value in env.items():
        saved[key] = os.environ.get(key)
        os.environ[key] = str(value)
    return saved


def _restore_env(saved: Dict[str, Optional[str]]) -> None:
    for key, value in saved.items():
        if value is None:
            os.environ.pop(key, None)
        else:
            os.environ[key] = value


# ============================================================================
# NDJSON TCP Server Implementation
# ============================================================================

# Tags of the engine's `MeasurementValue`. Inside one of them a null is
# not always valid (`Numeric` is an f64, axis data are numbers), and the
# engine would refuse the whole line over it.
_MEASUREMENT_VALUE_TAGS = frozenset(
    ("Boolean", "Numeric", "String", "Array", "MultiDimensional", "Object")
)


def _json_safe(value):
    """Last resort for a value that slipped past `_check_recordable`:
    non-finite floats become null, anything else unknown its str(). A
    tagged measurement value that is not valid JSON as it stands is
    recorded as `"Null"`, which the engine accepts, and logged."""
    value = _plain_json_value(value)
    if isinstance(value, float) and not math.isfinite(value):
        return None
    if value is None or isinstance(value, (str, bool, int, float)):
        return value
    if isinstance(value, dict):
        if len(value) == 1:
            tag, inner = next(iter(value.items()))
            if tag in _MEASUREMENT_VALUE_TAGS:
                try:
                    json.dumps(inner, allow_nan=False)
                    return {tag: inner}
                except (TypeError, ValueError):
                    print(
                        f"ERROR: a {tag} measurement value cannot be sent to the engine "
                        f"and was recorded as Null: {_short_repr(inner)}",
                        file=_original_stderr,
                        flush=True,
                    )
                    return "Null"
        return {str(k): _json_safe(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_json_safe(v) for v in value]
    return str(value)


def _dumps(obj) -> str:
    """json.dumps for everything sent to the engine. Never raises: an
    exception here ended the event loop while the phase thread ran on,
    and the engine then dispatched the next phase onto this worker."""
    try:
        return json.dumps(obj, allow_nan=False)
    except (TypeError, ValueError):
        return json.dumps(_json_safe(obj), allow_nan=False)


def _measurement_to_ndjson(m: dict) -> dict:
    """Convert internal measurement dict to NDJSON-compatible dict"""
    result = {
        "name": m["name"],
        "value": m.get("value"),
        "timestamp": m.get("timestamp", ""),
    }
    if "unit" in m and m["unit"]:
        result["unit"] = m["unit"]
    if "result" in m and m["result"]:
        result["result"] = m["result"]
    if "aggregations" in m and m["aggregations"]:
        serialized_aggs = []
        for agg in m["aggregations"]:
            serialized_agg = {"type": agg["type"]}
            if "value" in agg and agg["value"] is not None:
                serialized_agg["value"] = agg["value"]
            serialized_aggs.append(serialized_agg)
        result["aggregations"] = serialized_aggs
    return result


def _log_to_ndjson(log: dict) -> dict:
    """Convert internal log dict to NDJSON-compatible dict"""
    result = {
        "timestamp": log.get("timestamp", ""),
        "level": log.get("level", "INFO"),
        "message": log.get("message", ""),
    }
    if "file" in log and log["file"]:
        result["file"] = log["file"]
    if "line" in log and log["line"]:
        result["line"] = log["line"]
    return result


def handle_connection(conn: socket.socket, procedure_dir: Path):
    """Handle a single TCP connection: read command, stream events, write result."""
    try:
        # Read command (one JSON line)
        buf = b""
        while b"\n" not in buf:
            chunk = conn.recv(65536)
            if not chunk:
                return
            buf += chunk

        line, rest = buf.split(b"\n", 1)
        command = json.loads(line.decode("utf-8"))

        # The engine writes on this connection again while the phase runs
        # (its replies to `ui.wait()`): read them on a thread of their own,
        # the main thread is busy streaming events out.
        engine = _EngineChannel()
        threading.Thread(
            target=_read_engine_messages,
            args=(conn, rest, engine),
            daemon=True,
        ).start()

        # Acknowledge the job BEFORE any user code can run. This is the
        # liveness handshake the Rust side times: no ack within its
        # deadline means this interpreter is wedged (e.g. suspended by an
        # endpoint-protection agent), which phase timeouts cannot catch
        # because they are enforced from this same process.
        ack = {"type": "JobAck", "job_id": command.get("job_id", "")}
        conn.sendall((_dumps(ack) + "\n").encode("utf-8"))

        # Parse phase_results: values are JSON strings that need deserialization
        phase_results_raw = command.get("phase_results", {})
        phase_results = {}
        for k, v in phase_results_raw.items():
            try:
                phase_results[k] = json.loads(v) if isinstance(v, str) else v
            except (json.JSONDecodeError, TypeError):
                phase_results[k] = v
        command["phase_results"] = phase_results

        try:
            final_result = None

            for item in execute_job_streaming(command, procedure_dir, engine):
                if item["type"] == "event":
                    event = item["data"]
                    if event["type"] == "attach_file":
                        msg = {
                            "type": "AttachFile",
                            "job_id": event["job_id"],
                            "source_path": event["source_path"],
                            "attachment_name": event["attachment_name"],
                        }
                        conn.sendall((_dumps(msg) + "\n").encode("utf-8"))
                    elif event["type"] == "attach_data":
                        msg = {
                            "type": "AttachData",
                            "job_id": event["job_id"],
                            "data": event["data"],
                            "attachment_name": event["attachment_name"],
                        }
                        conn.sendall((_dumps(msg) + "\n").encode("utf-8"))
                    elif event["type"] == "ModuleLoaded":
                        msg = {
                            "type": "ModuleLoaded",
                            "job_id": event["job_id"],
                        }
                        conn.sendall((_dumps(msg) + "\n").encode("utf-8"))
                    elif event["type"] == "ui_update":
                        msg = {
                            "type": "UiUpdate",
                            "job_id": event["job_id"],
                            "action": event["action"],
                            "data_json": _dumps(event["data"]),
                        }
                        conn.sendall((_dumps(msg) + "\n").encode("utf-8"))
                    elif event["type"] == "PhaseLogLine":
                        msg = {
                            "type": "PhaseLogLine",
                            "job_id": event["job_id"],
                            "level": event["level"],
                            "message": event["message"],
                            "timestamp": event["timestamp"],
                        }
                        if event.get("file"):
                            msg["file"] = event["file"]
                        if event.get("line"):
                            msg["line"] = event["line"]
                        conn.sendall((_dumps(msg) + "\n").encode("utf-8"))
                    elif event["type"] == "ui_wait":
                        msg = {
                            "type": "UiWait",
                            "job_id": event["job_id"],
                            "wait_id": event["wait_id"],
                        }
                        conn.sendall((_dumps(msg) + "\n").encode("utf-8"))
                    elif event["type"] == "MeasurementRecorded":
                        msg = {
                            "type": "MeasurementRecorded",
                            "job_id": event["job_id"],
                            "name": event["name"],
                            "value_json": _dumps(event["value"]),
                            "timestamp": event["timestamp"],
                        }
                        if event.get("unit"):
                            msg["unit"] = event["unit"]
                        conn.sendall((_dumps(msg) + "\n").encode("utf-8"))
                elif item["type"] == "result":
                    final_result = item["data"]

            if final_result:
                job_result = {
                    "type": "JobComplete",
                    "success": final_result["success"],
                    "measurements": [
                        _measurement_to_ndjson(m)
                        for m in final_result.get("measurements", [])
                    ],
                    "logs": [
                        _log_to_ndjson(log)
                        for log in final_result.get("logs", [])
                    ],
                    "error": final_result.get("error_message") or final_result.get("error"),
                    "exit_code": final_result.get("exit_code"),
                }

                if "phase_result" in final_result and final_result["phase_result"] is not None:
                    job_result["phase_result_json"] = _dumps(final_result["phase_result"])

                if "unit" in final_result and final_result["unit"] is not None:
                    job_result["unit_json"] = _dumps(final_result["unit"])

                if final_result.get("run_metadata"):
                    job_result["run_metadata_json"] = _dumps(
                        final_result["run_metadata"]
                    )

                if final_result.get("unit_metadata"):
                    job_result["unit_metadata_json"] = _dumps(
                        final_result["unit_metadata"]
                    )

                if final_result.get("locals"):
                    job_result["locals_json"] = _dumps(final_result["locals"])

                answered = engine.answered_wait_id()
                if answered is not None:
                    job_result["ui_answered_wait_id"] = answered

                conn.sendall((_dumps(job_result) + "\n").encode("utf-8"))

        except Exception as e:
            error_msg = f"Worker error: {str(e)}"
            print(f"ERROR: {error_msg}", file=_original_stderr)
            msg = {"type": "Error", "message": error_msg}
            conn.sendall((_dumps(msg) + "\n").encode("utf-8"))

    except Exception as e:
        print(f"Connection handler error: {e}", file=_original_stderr)
    finally:
        # shutdown() before close(): a thread blocked in recv() on this
        # socket (the engine-message reader) is not woken by close() on
        # every platform. Waking it is what releases a phase thread still
        # blocked in ui.wait() after a timeout.
        try:
            conn.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        conn.close()


def _start_parent_watchdog(parent_pid: Optional[int] = None):
    """Exit when the spawning CLI process dies.

    The worker is a TCP server with no stdin pipe to the parent, so a
    SIGKILLed parent (crash, force-quit, watchdog) otherwise leaves the
    worker running forever. Unix: poll getppid() — the kernel reparents
    orphans to pid 1 (or a subreaper), so a change means the parent is
    gone. Windows: hold a SYNCHRONIZE handle to the parent and wait on
    it; the wait returns when the process exits.

    `parent_pid` is the pid the spawner passed on the command line. It
    must not be read with getppid() here: if the parent died before this
    line ran, getppid() already returns the reaper's pid and the watchdog
    would never fire (TP-1168, first seen in tp_plug.py). The getppid()
    fallback is for callers that predate `--parent-pid`. Same shape as
    tp_plug.py's watchdog; keep the two in step.
    """
    if parent_pid is None:
        parent_pid = os.getppid()
        # Loud on purpose: this path is the TP-1168 race. Every in-tree
        # spawner passes --parent-pid, so this line in a log means a spawn
        # site dropped the flag, and nothing else would notice until the
        # next orphan.
        print(
            "parent watchdog: no --parent-pid given, falling back to getppid() (racy, TP-1168)",
            file=_original_stderr,
            flush=True,
        )

    if os.name == "nt":
        import ctypes
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        # Declared, not defaulted: OpenProcess returns a HANDLE and
        # WaitForSingleObject a DWORD; as c_int, WAIT_FAILED (0xFFFFFFFF)
        # would come back as -1.
        kernel32.OpenProcess.restype = ctypes.c_void_p
        kernel32.OpenProcess.argtypes = [ctypes.c_uint32, ctypes.c_int, ctypes.c_uint32]
        kernel32.WaitForSingleObject.restype = ctypes.c_uint32
        kernel32.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
        SYNCHRONIZE = 0x00100000
        WAIT_OBJECT_0 = 0x00000000
        INFINITE = 0xFFFFFFFF
        ERROR_INVALID_PARAMETER = 87
        # Both "already dead" checks run on the caller's thread, before
        # serve() announces the port. A pid with no process object fails
        # OpenProcess with ERROR_INVALID_PARAMETER; an object already
        # signalled is a parent that exited while something held it.
        handle = kernel32.OpenProcess(SYNCHRONIZE, False, parent_pid)
        if not handle:
            err = ctypes.get_last_error()
            if err == ERROR_INVALID_PARAMETER:
                os._exit(0)
            # Not evidence of death (access denied, most plausibly): keep
            # serving unwatched, as before, but say so instead of the old
            # silent disarm.
            print(
                f"parent watchdog not armed: OpenProcess({parent_pid}) failed with Win32 error {err}",
                file=_original_stderr,
                flush=True,
            )
            return
        if kernel32.WaitForSingleObject(handle, 0) == WAIT_OBJECT_0:
            os._exit(0)

        def wait_windows():
            rc = kernel32.WaitForSingleObject(handle, INFINITE)
            if rc == WAIT_OBJECT_0:
                os._exit(0)
            # WAIT_FAILED: the handle stopped being waitable while the
            # parent may well be alive. Same reading as an OpenProcess
            # failure above, not evidence of death: keep serving,
            # unwatched, and say so rather than end a live run.
            try:
                print(
                    f"parent watchdog stopped: WaitForSingleObject returned {rc}, "
                    f"Win32 error {ctypes.get_last_error()}",
                    file=_original_stderr,
                    flush=True,
                )
            except OSError:
                pass

        threading.Thread(target=wait_windows, daemon=True).start()
        return

    if os.getppid() != parent_pid:
        os._exit(0)

    def poll_unix():
        while True:
            if os.getppid() != parent_pid:
                os._exit(0)
            time.sleep(2)

    threading.Thread(target=poll_unix, daemon=True).start()


def serve(procedure_dir: Path):
    """Start NDJSON TCP server and print port to stdout"""
    original_stdout = sys.stdout

    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", 0))
    port = server.getsockname()[1]
    server.listen(5)

    # Print port for Rust to discover
    print(f"NDJSON_PORT:{port}", flush=True, file=original_stdout)
    # The engine stops reading stdout after the port line. Between jobs
    # (a phase's background thread, a C library) a write there hit a
    # dead pipe and raised BrokenPipeError: point fd 1 and sys.stdout at
    # stderr, which is read for the worker's whole life.
    try:
        os.dup2(2, 1)
    except OSError:
        pass
    sys.stdout = sys.stderr

    # Start the debugger listener AFTER the port handshake: the Rust side
    # reads NDJSON_PORT before connecting, and per-job connections queue in
    # the listen() backlog while we block in wait_for_client().
    _maybe_start_debugpy()

    try:
        while True:
            conn, addr = server.accept()
            # Handle each connection in a thread
            t = threading.Thread(
                target=handle_connection,
                args=(conn, procedure_dir),
                daemon=True,
            )
            t.start()
    except KeyboardInterrupt:
        print("Worker interrupted", file=_original_stderr)
    finally:
        server.close()


_PARAM_KIND_NAMES = {
    inspect.Parameter.POSITIONAL_OR_KEYWORD: "positional_or_keyword",
    inspect.Parameter.KEYWORD_ONLY: "keyword_only",
    inspect.Parameter.POSITIONAL_ONLY: "positional_only",
    inspect.Parameter.VAR_POSITIONAL: "var_positional",
    inspect.Parameter.VAR_KEYWORD: "var_keyword",
}


def _describe_plug_param(param: inspect.Parameter, positional_call: bool = False) -> dict:
    """`positional_call`: the parameter belongs to a method a phase reaches
    through `Plug`, so the kind reported is the one that call accepts,
    not the one the `def` line declares. A `positional_or_keyword`
    parameter is `positional_only` from the phase's side, and a
    `keyword_only` one cannot be passed at all (reported as is; the
    caller has no way to reach it). `__init__` is the exception: the
    runner constructs the class in-process with the YAML config as
    keyword arguments, so its declared kinds stand."""
    is_var = param.kind in (
        inspect.Parameter.VAR_POSITIONAL,
        inspect.Parameter.VAR_KEYWORD,
    )
    kind = _PARAM_KIND_NAMES[param.kind]
    if positional_call and param.kind is inspect.Parameter.POSITIONAL_OR_KEYWORD:
        kind = "positional_only"
    has_default = param.default is not inspect.Parameter.empty
    default_json = None
    if has_default:
        try:
            default_json = json.dumps(param.default)
        except (TypeError, ValueError):
            # Non-JSON default (an object, a set, ...): report the
            # parameter as optional but leave the value unstated.
            default_json = None
    annotation = None
    if param.annotation is not inspect.Parameter.empty:
        ann = param.annotation
        annotation = ann.__name__ if isinstance(ann, type) else str(ann)
    return {
        "name": param.name,
        "kind": kind,
        # As the `def` line declares it: what a direct call (`tofupilot plug
        # call --kwargs`) accepts.
        "declared_kind": _PARAM_KIND_NAMES[param.kind],
        "required": not has_default and not is_var,
        "default_json": default_json,
        "annotation": annotation,
    }


def _describe_plug_methods(cls) -> list:
    """The methods a plug call can reach: the class's own, then those it
    inherits (a simulator subclassing the driver), staticmethods and
    classmethods included, `object`'s left out. Each class's in source
    order; a name defined lower in the hierarchy hides the one above.
    `__init__` and underscore methods are listed, the caller decides."""
    methods = []
    seen = set()
    for depth, klass in enumerate(cls.__mro__):
        if klass is object:
            continue
        own = []
        for name, raw in vars(klass).items():
            if name in seen:
                continue
            # Whatever the class defines under a name hides the parent's
            # method of that name, a property or a `None` included.
            seen.add(name)
            if isinstance(raw, (staticmethod, classmethod)):
                function = raw.__func__
                # Bound through the class: `cls` drops out, as at the call.
                signature_of = getattr(cls, name)
                skip_first = False
            elif inspect.isfunction(raw):
                function = raw
                signature_of = raw
                skip_first = True
            else:
                continue
            # A decorated method: its own `def` line, not the decorator's
            # wrapper.
            function = inspect.unwrap(function)
            doc = None
            if function.__doc__:
                doc = next(
                    (line.strip() for line in function.__doc__.splitlines() if line.strip()),
                    None,
                )
            params = list(inspect.signature(signature_of).parameters.items())
            if skip_first and params:
                params = params[1:]
            own.append(
                {
                    "name": name,
                    "doc": doc,
                    "lineno": function.__code__.co_firstlineno,
                    # The class that defines it: a line of the parent's file
                    # for an inherited method.
                    "file": function.__code__.co_filename,
                    "depth": depth,
                    "params": [
                        _describe_plug_param(
                            param,
                            positional_call=(
                                name != "__init__" and not PLUG_CALL_ACCEPTS_KEYWORDS
                            ),
                        )
                        for _, param in params
                    ],
                }
            )
        own.sort(key=lambda m: m["lineno"])
        methods.extend(own)
    for m in methods:
        del m["depth"]
    return methods


def run_introspection(procedure_dir: Path):
    """--introspect mode: report each requested phase's real parameter
    names before the job graph is built.

    Reads one JSON object from stdin:
        {"phases": {"<phase_key>": {"module": "...", "function": "..."}},
         "plugs": {"<key>": {"module": "...", "class": "..."}}}
    ("plugs" is optional), imports each callable exactly the way
    execution would (load_phase_module), and prints one JSON object to
    stdout:
        {"phases": {"<phase_key>": {"params": [...]} | {"error": "..."}},
         "plugs": {"<key>": {"methods": [...]} | {"error": "..."}}}
    with "plugs" present only when requested.

    Per-entry errors are reported, not fatal: a phase whose module won't
    import would fail at run time anyway, and if it isn't in the partial
    set its import error must not stop the run from starting.
    """
    request = json.load(sys.stdin)

    # Importing phase modules runs their module-level code, which may
    # print. Keep stdout clean for the JSON contract with the Rust side.
    out_stream = sys.stdout
    sys.stdout = sys.stderr

    results = {}
    plug_results = {}
    plug_requests = request.get("plugs") or {}
    try:
        for phase_key, spec in (request.get("phases") or {}).items():
            try:
                module_name = spec.get("module", spec.get("file", ""))
                function_name = spec["function"]
                module = load_phase_module(module_name, procedure_dir)
                if not hasattr(module, function_name):
                    raise AttributeError(
                        f"Function {function_name} not found in {module_name}"
                    )
                sig = inspect.signature(getattr(module, function_name))
                results[phase_key] = {"params": list(sig.parameters)}
            except BaseException as exc:
                results[phase_key] = {"error": str(exc) or type(exc).__name__}
        for plug_key, spec in plug_requests.items():
            try:
                module_name = spec["module"]
                class_name = spec["class"]
                module = load_phase_module(module_name, procedure_dir)
                if not hasattr(module, class_name):
                    raise AttributeError(
                        f"Class {class_name} not found in {module_name}"
                    )
                cls = getattr(module, class_name)
                plug_results[plug_key] = {"methods": _describe_plug_methods(cls)}
            except BaseException as exc:
                plug_results[plug_key] = {"error": str(exc) or type(exc).__name__}
    finally:
        sys.stdout = out_stream

    output = {"phases": results}
    if plug_requests:
        output["plugs"] = plug_results
    print(json.dumps(output), flush=True)


def main():
    args = [a for a in sys.argv[1:] if a != "--introspect"]
    introspect_mode = len(args) != len(sys.argv) - 1

    # `--parent-pid N`: the spawner's pid for the parent watchdog. Optional
    # so introspection (which never arms the watchdog) and older spawners
    # keep working; see _start_parent_watchdog for why it is passed in.
    parent_pid = None
    if "--parent-pid" in args:
        at = args.index("--parent-pid")
        try:
            parent_pid = int(args[at + 1])
        except (IndexError, ValueError):
            print("Usage: tp_worker.py <procedure_dir> [--introspect] [--parent-pid N]", file=sys.stderr)
            sys.exit(1)
        del args[at : at + 2]

    if len(args) != 1:
        print("Usage: tp_worker.py <procedure_dir> [--introspect] [--parent-pid N]", file=sys.stderr)
        sys.exit(1)

    procedure_dir = Path(args[0]).resolve()
    if not procedure_dir.exists():
        print(f"Error: Procedure directory not found: {procedure_dir}", file=sys.stderr)
        sys.exit(1)

    # Put the procedure root on sys.path so phase modules can import
    # sibling packages (utils/, drivers/, helpers/) by name. Without
    # this, `spec_from_file_location` loads `phases/foo.py` in isolation
    # and any `from utils.bar import …` raises ModuleNotFoundError.
    procedure_dir_str = str(procedure_dir)
    if procedure_dir_str not in sys.path:
        sys.path.insert(0, procedure_dir_str)

    if introspect_mode:
        run_introspection(procedure_dir)
        return

    _start_parent_watchdog(parent_pid)
    serve(procedure_dir)


if __name__ == "__main__":
    main()
