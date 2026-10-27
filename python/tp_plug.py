#!/usr/bin/env python3
"""
Per-Plug Service - NDJSON TCP implementation.
No external dependencies beyond the Python standard library.
"""

import importlib
import importlib.util
import inspect
import os
import sys
import traceback
import time
import functools
import gc
import types
import weakref
import json
import logging
import threading
import select
import socket
from typing import Dict, Any, Optional
from pathlib import Path

# Save original stderr before any redirection
_original_stderr = sys.__stderr__


# ============================================================================
# Logging (inlined from tp_logs.py)
# ============================================================================


# ============================================================================
# Dashboard access (the `dashboard` parameter of a phase or plug)
# ============================================================================



class _PlugConfigError(Exception):
    """The plug's reference or `with:` does not fit its class (no such
    class, an argument `__init__` does not take): said in one line, the
    traceback would only show this service's own frames."""


def _plug_traceback(e: BaseException) -> str:
    """The traceback of a plug's exception without this service's frames
    nor the import machinery's: the same on every failure, they point at
    nothing the plug's author can fix. Chained causes are kept, filtered
    the same way. A traceback with no frame of the plug's is this
    service's own failure: its frames are kept, they are the only ones
    pointing at the bug. `tp_worker._phase_traceback` is the same filter:
    each script is extracted on its own, under a hashed name, so neither
    can import the other."""
    here = os.path.abspath(__file__)

    def ours(filename: str) -> bool:
        return os.path.abspath(filename) == here or filename.startswith("<frozen importlib")

    te = traceback.TracebackException.from_exception(e)
    node, seen = te, set()
    while node is not None and id(node) not in seen:
        seen.add(id(node))
        kept = [f for f in node.stack if not ours(f.filename)]
        if kept:
            node.stack = traceback.StackSummary.from_list(kept)
        node = node.__cause__ or (
            None if node.__suppress_context__ else node.__context__
        )
    return "".join(te.format())

_MISSING = object()


def _call_mismatch(method, args, kwargs) -> Optional[str]:
    """Why `method(*args, **kwargs)` cannot be called, before calling it, as
    Python words it (`missing a required argument: 'v'`), or None. The call
    itself would raise the same TypeError, with a traceback of this
    service's frames only: nothing in it points at the caller's mistake.
    A method without a readable signature (a C extension's) is left to the
    call."""
    try:
        signature = inspect.signature(method)
    except (TypeError, ValueError):
        return None
    try:
        signature.bind(*args, **kwargs)
    except TypeError as e:
        return str(e)
    return None


def _error_reply(e: BaseException, error: Optional[str] = None) -> Dict[str, Any]:
    """A failed call: its traceback (the exception line alone when none of
    its frames is the plug's), and the exception's class, so the phase that
    called can raise one it catches by type (`except ValueError`)."""
    return {
        "success": False,
        "error": error if error is not None else "".join(traceback.format_exception_only(type(e), e)),
        "error_module": type(e).__module__,
        "error_qualname": type(e).__qualname__,
        "error_message": str(e),
    }


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


# Reentrant for the same reason as the capture stream's lock: a GC
# `__del__` that prints can land here mid-write on the same thread.
_stderr_lock = threading.RLock()


def _emit_log_entry(entry: Dict[str, Any]):
    """One JSON log line on the real stderr, which the engine reads and
    turns into plug log events. Nothing is kept in memory: a station
    plug lives for days, and a list nobody read only grew."""
    if not _original_stderr:
        return
    line = json.dumps(entry, default=str) + "\n"
    with _stderr_lock:
        try:
            _original_stderr.write(line)
            _original_stderr.flush()
        except Exception:
            pass


def _log_entry(level: str, message: str, file_path=None, line_number=None) -> Dict[str, Any]:
    from datetime import datetime, timezone

    entry = {
        "timestamp": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "level": level,
        "message": message,
    }
    if file_path:
        entry["file"] = file_path
    if line_number:
        entry["line"] = line_number
    return entry


class Logs:
    def __init__(self, job_id=None):
        self.job_id = job_id

    def _add_log(self, level: str, message: str):
        frame = inspect.currentframe()
        caller_frame = (
            frame.f_back.f_back
            if frame and frame.f_back and frame.f_back.f_back
            else None
        )

        file_path = None
        line_number = None
        if caller_frame:
            file_path = caller_frame.f_code.co_filename
            line_number = caller_frame.f_lineno
            try:
                if file_path.startswith("/"):
                    base_dir = os.path.dirname(
                        os.path.dirname(os.path.abspath(__file__))
                    )
                    if file_path.startswith(base_dir):
                        file_path = os.path.relpath(file_path, base_dir)
            except Exception:
                pass

        _emit_log_entry(_log_entry(level, message, file_path, line_number))

    def info(self, message: str):
        self._add_log("INFO", message)

    def warning(self, message: str):
        self._add_log("WARNING", message)

    def error(self, message: str):
        self._add_log("ERROR", message)

    def debug(self, message: str):
        # The service's own trace (start, cleanup, shutdown), not the
        # plug's: marked so the engine keeps it off the plug's log.
        _emit_log_entry({**_log_entry("DEBUG", message), "service": True})


def _relativize_log_path(file_path):
    """Same as the worker's: strip the engine's base directory so a log
    origin renders project-relative."""
    try:
        if file_path and file_path.startswith("/"):
            base_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
            if file_path.startswith(base_dir):
                return os.path.relpath(file_path, base_dir)
    except Exception:
        pass
    return file_path


class StdlibLoggingBridge(logging.Handler):
    """Forwards a plug's stdlib `logging` records as log entries with
    their level, file and line, as the worker does for a phase. Without
    it a record reaches the last-resort handler on stderr and arrives as
    a WARNING line with no source."""

    def emit(self, record):
        try:
            _emit_log_entry(
                _log_entry(
                    record.levelname,
                    self.format(record),
                    _relativize_log_path(record.pathname),
                    record.lineno,
                )
            )
        except Exception:
            pass


class LogCapturingStream:
    """sys.stdout / sys.stderr of the plug process: each complete line
    becomes one log entry at `level`."""

    encoding = "utf-8"
    errors = "replace"

    def __init__(self, level="INFO"):
        self.level = level
        self.buffer = ""
        # Handler threads and a running method may print concurrently.
        # Reentrant: a `__del__` that prints, run by the GC in the middle
        # of a write on the same thread, would otherwise deadlock.
        self._lock = threading.RLock()

    def write(self, text):
        with self._lock:
            self.buffer += text
            lines = []
            while "\n" in self.buffer:
                line, self.buffer = self.buffer.split("\n", 1)
                if line.strip():
                    lines.append(line.strip())
        for line in lines:
            _emit_log_entry(_log_entry(self.level, line))
        return len(text)

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


# ============================================================================
# Plug Service
# ============================================================================

class PlugHandler:
    """Handles plug lifecycle and method calls"""

    def __init__(
        self,
        procedure_dir: Path,
        plug_key: str,
        display_name: str,
        plug_config: Dict[str, Any],
    ):
        self.procedure_dir = procedure_dir
        self.plug_key = plug_key
        self.display_name = display_name
        self.plug_config = plug_config
        self.plug_instance = None
        # (loop, thread) of an async driver: one loop for the plug's life,
        # running on its own thread, so a connection one call opens is
        # usable by the next and a reader task runs between calls.
        self._loop = None
        self._initialized = False
        self._init_error = None
        self._init_start_time = time.time()
        # One method call at a time on the instance, like a SCPI socket.
        # Control requests (GetStatus, Cleanup, Shutdown) do NOT take
        # this lock, so a long or abandoned method never blocks the
        # engine's health probe or its shutdown sequence.
        self._call_lock = threading.Lock()
        # Set once the service must stop accepting connections: after a
        # Shutdown, or after a Cleanup on an initialized plug.
        self.shutdown_event = threading.Event()
        # serve() waits for an in-flight Cleanup before exiting so
        # interpreter teardown cannot cut __del__ short.
        self.cleanup_started = threading.Event()
        self.cleanup_done = threading.Event()

        self.logs = Logs()

        # Every print becomes a JSON log line on the real stderr.
        sys.stdout = LogCapturingStream(level="INFO")
        sys.stderr = LogCapturingStream(level="WARNING")
        # A plug's `logging` records keep their level and source, as a
        # phase's do. The root's default WARNING threshold would drop INFO
        # before the bridge; loggers with their own level still filter.
        bridge = StdlibLoggingBridge()
        bridge.setLevel(logging.INFO)
        root_logger = logging.getLogger()
        root_logger.addHandler(bridge)
        if root_logger.getEffectiveLevel() > logging.INFO:
            root_logger.setLevel(logging.INFO)

        # Add procedure directory to path
        if str(procedure_dir) not in sys.path:
            sys.path.insert(0, str(procedure_dir))

        # Start background initialization
        self._init_thread = threading.Thread(target=self._background_init, daemon=True)
        self._init_thread.start()

    def _background_init(self):
        try:
            self._create_plug_instance()
            self._initialized = True
        except Exception as e:
            self._init_error = str(e)
            detail = str(e) if isinstance(e, _PlugConfigError) else _plug_traceback(e)
            error_msg = f"Failed to initialize plug {self.display_name}:\n{detail}"
            self.logs.error(error_msg)

    def _create_plug_instance(self):
        file_path = self.plug_config.get("file")
        module_ref = self.plug_config.get("module")
        class_name = self.plug_config.get("class")

        if not file_path and not module_ref:
            raise ValueError("Plug config must contain a 'file' or 'module' field")
        if not class_name:
            raise ValueError("Plug config must contain 'class' field")

        if module_ref:
            # A class of an installed package (a vendor driver): imported
            # by name from the venv, as a phase outside the tree is.
            module = importlib.import_module(module_ref)
        else:
            file_path = Path(file_path)
            if not file_path.exists():
                raise FileNotFoundError(f"Plug file not found: {file_path}")

            # Inside a package of the procedure tree, its dotted name
            # (`drivers.psu`), so its relative imports (`from .base import
            # ScpiBase`) find their package; the procedure root is on
            # sys.path for that package to import.
            module_name = f"_plug_{file_path.stem}_{id(self)}"
            try:
                rel = file_path.resolve().relative_to(Path(self.procedure_dir).resolve())
                parts = list(rel.with_suffix("").parts)
                if parts and parts[-1] == "__init__":
                    parts.pop()
                if len(parts) >= 2 and all(p.isidentifier() for p in parts):
                    module_name = ".".join(parts)
                    root = str(Path(self.procedure_dir).resolve())
                    if root not in sys.path:
                        sys.path.insert(0, root)
            except ValueError:
                pass

            module = None
            if not module_name.startswith("_plug_"):
                # Imported as Python does, parents first: a package whose
                # `__init__` re-exports the class (`from .dmm import DMM`)
                # must not see it half-loaded. Another file under the same
                # name (an installed package) falls back to the file.
                try:
                    imported = importlib.import_module(module_name)
                except ImportError as e:
                    # The name resolves elsewhere (an installed package of
                    # the same top name, a folder with no `__init__.py`):
                    # the file is loaded on its own below. An import the
                    # plug's own code makes fails as it should.
                    if not (e.name and module_name.startswith(e.name)):
                        raise
                    imported = None
                if imported is not None and os.path.realpath(
                    getattr(imported, "__file__", "") or ""
                ) == os.path.realpath(file_path):
                    module = imported
            if module is None:
                if module_name in sys.modules:
                    module_name = f"_plug_{file_path.stem}_{id(self)}"
                spec = importlib.util.spec_from_file_location(module_name, file_path)
                if spec is None or spec.loader is None:
                    raise ImportError(f"Cannot load module from {file_path}")

                module = importlib.util.module_from_spec(spec)
                sys.modules[module_name] = module
                spec.loader.exec_module(module)

        plug_class = getattr(module, class_name, None)
        if plug_class is None:
            import difflib

            near = difflib.get_close_matches(
                class_name, [n for n in dir(module) if not n.startswith("_")], n=1
            )
            raise _PlugConfigError(
                f"no `{class_name}` in {module_ref or file_path}"
                + (f"; did you mean `{near[0]}`?" if near else "")
            )
        config = dict(self.plug_config.get("config") or {})
        # A plug whose __init__ declares `dashboard` receives the API
        # client, like a phase does. The loader refuses a `config:`
        # entry of that name, so nothing here can be overridden.
        if "dashboard" not in config:
            try:
                params = inspect.signature(plug_class.__init__).parameters
            except (TypeError, ValueError):
                params = {}
            if "dashboard" in params:
                config["dashboard"] = dashboard_client()
        try:
            self.plug_instance = plug_class(**config)
        except TypeError as e:
            # Raised by the call itself (a keyword `__init__` does not
            # take, one it needs): no frame of the plug's own code.
            here = os.path.abspath(__file__)
            frames = traceback.extract_tb(e.__traceback__)
            if all(os.path.abspath(f.filename) == here for f in frames):
                raise _PlugConfigError(f"{class_name}(): {e}") from None
            raise

    def _get_plug_state(self) -> Dict[str, str]:
        if not self.plug_instance:
            return {}

        state = {}
        for attr_name in dir(self.plug_instance):
            if attr_name.startswith("_"):
                continue
            try:
                # Read without running anything: a property can talk to the
                # instrument (`*IDN?`), and the state is read after every call.
                value = inspect.getattr_static(self.plug_instance, attr_name)
                # A `__slots__` field: reading it runs no Python code.
                if isinstance(value, types.MemberDescriptorType):
                    value = getattr(self.plug_instance, attr_name)
                if not callable(value) and isinstance(
                    value, (str, int, float, bool, type(None))
                ):
                    state[attr_name] = str(value)
            except Exception:
                pass

        return state

    def handle_request(
        self, request: Dict[str, Any], conn: "socket.socket | None" = None
    ) -> Dict[str, Any]:
        """Handle a single NDJSON request and return a response dict."""
        req_type = request.get("type", "")

        if req_type == "CallMethod":
            return self._handle_call_method(request, conn)
        elif req_type == "GetStatus":
            return self._handle_get_status()
        elif req_type == "Cleanup":
            return self._handle_cleanup()
        elif req_type == "Shutdown":
            return self._handle_shutdown()
        else:
            return {"success": False, "error": f"Unknown request type: {req_type}"}

    @staticmethod
    def _caller_gone(conn) -> bool:
        """True when the worker already closed its end (phase deadline hit,
        worker killed). A queued request whose caller is gone must not run:
        the phase already failed, and firing an instrument write minutes
        later is worse than dropping it."""
        if conn is None:
            return False
        try:
            readable, _, _ = select.select([conn], [], [], 0)
            if not readable:
                return False
            return conn.recv(1, socket.MSG_PEEK) == b""
        except OSError:
            return True

    def _handle_call_method(
        self, request: Dict[str, Any], conn=None
    ) -> Dict[str, Any]:
        try:
            # Wait for background initialization
            if self._init_thread and self._init_thread.is_alive():
                self._init_thread.join(timeout=30)
                if self._init_thread.is_alive():
                    return {"success": False, "error": "Plug initialization timeout after 30s"}

            if self._init_error:
                return {"success": False, "error": f"Plug initialization failed: {self._init_error}"}

            if not self.plug_instance:
                return {"success": False, "error": "Plug not initialized"}

            method_name = request.get("method", "")
            # Looked up statically: `hasattr` runs a property, and a
            # property that queries the instrument is no method to call.
            static = inspect.getattr_static(self.plug_instance, method_name, _MISSING)
            # A property, or a `cached_property` not computed yet (reading
            # it runs the query); not a `__slots__` field, which may hold
            # a callable.
            is_property = (
                inspect.isdatadescriptor(static)
                and not inspect.isroutine(static)
                and not isinstance(static, types.MemberDescriptorType)
            ) or isinstance(static, functools.cached_property)
            if is_property:
                return {
                    "success": False,
                    "error": f"{method_name} is a property of {self.display_name}, not a method: "
                    "read it through a method",
                }
            if static is _MISSING and not hasattr(type(self.plug_instance), "__getattr__"):
                return {
                    "success": False,
                    "error": f"Method {method_name} not found on {self.display_name}",
                }
            if not hasattr(self.plug_instance, method_name):
                return {
                    "success": False,
                    "error": f"Method {method_name} not found on {self.display_name}",
                }

            args_json = request.get("args_json", "")
            args = json.loads(args_json) if args_json else []
            kwargs_json = request.get("kwargs_json", "")
            kwargs = json.loads(kwargs_json) if kwargs_json else {}

            abandoned = {
                "success": False,
                "error": f"{method_name} skipped: caller closed the connection before it ran",
            }
            # Checked before waiting for the lock and again after getting
            # it: the wait can be long when many slots share the plug.
            if self._caller_gone(conn):
                return abandoned
            with self._call_lock:
                if self._caller_gone(conn):
                    return abandoned
                # Bound the method only now: a Cleanup that won the lock
                # first has dropped the instance, and a reference taken
                # earlier would keep it alive past its __del__.
                instance = self.plug_instance
                if instance is None:
                    return {
                        "success": False,
                        "error": f"{method_name} skipped: plug {self.display_name} was cleaned up",
                    }
                method = getattr(instance, method_name)
                if not callable(method):
                    return {
                        "success": False,
                        "error": f"{method_name} is a {type(method).__name__} attribute of "
                        f"{self.display_name}, not a method",
                    }
                mismatch = _call_mismatch(method, args, kwargs)
                if mismatch is not None:
                    return _error_reply(TypeError(mismatch))
                result = method(*args, **kwargs)
                # An `async def` method of an async driver: run to completion
                # on the plug's loop, which keeps running between calls so a
                # reader or keepalive task the driver started goes on.
                if inspect.iscoroutine(result):
                    result = self._run_async(result)
                state = self._get_plug_state()

            try:
                result_json = json.dumps(result) if result is not None else None
            except (TypeError, ValueError) as e:
                # The method ran, its side effects happened: only the return
                # value cannot reach a phase. A bare traceback read as if the
                # method itself had failed. Deliberately an error and not a
                # stand-in value, debugger included: the debugger must fail
                # where a run fails.
                message = (
                    f"{self.display_name}.{method_name}() ran, but its return value "
                    f"cannot be sent to a phase: {e}. Return a JSON type instead "
                    "(str, int, float, bool, list, dict or None): x.item() for a numpy "
                    "scalar, x.tolist() for a small numpy array, "
                    "base64.b64encode(data).decode() for bytes (encode an image frame "
                    "to PNG or JPEG bytes first), x.isoformat() for a datetime."
                )
                self.logs.error(message)
                return {"success": False, "error": message}

            return {
                "success": True,
                "result_json": result_json,
                "state": state,
            }

        except Exception as e:
            error_msg = _plug_traceback(e)
            # The traceback travels in the reply, and the caller reports it:
            # a phase that catches the exception has no error to show, and
            # `plug call` prints the traceback. A trace line only.
            self.logs.debug(
                f"Failed to call {self.display_name}.{request.get('method', '?')}: "
                f"{type(e).__name__}: {e}"
            )
            return _error_reply(e, error_msg)

    def _handle_get_status(self) -> Dict[str, Any]:
        if self._init_thread and self._init_thread.is_alive():
            init_duration = time.time() - self._init_start_time
            # The engine owns the readiness deadline and reads this flag,
            # never the text: a plug's own error may say "initializing".
            return {
                "success": False,
                "initializing": True,
                "error": f"Plug initializing... ({init_duration:.1f}s elapsed)",
            }

        if self._init_error:
            return {"success": False, "error": f"Plug initialization failed: {self._init_error}"}

        if not self.plug_instance:
            return {"success": False, "error": "Plug not initialized"}

        # Health probe must answer even while a method runs; the state
        # snapshot is best effort and skipped when the instance is busy.
        state: Dict[str, str] = {}
        if self._call_lock.acquire(blocking=False):
            try:
                state = self._get_plug_state()
            finally:
                self._call_lock.release()
        return {"success": True, "state": state}

    def _handle_cleanup(self) -> Dict[str, Any]:
        self.cleanup_started.set()
        try:
            return self._cleanup()
        finally:
            self.cleanup_done.set()

    def _run_async(self, coroutine):
        import asyncio

        if self._loop is None:
            loop = asyncio.new_event_loop()
            thread = threading.Thread(
                target=loop.run_forever, name=f"{self.display_name}-asyncio", daemon=True
            )
            thread.start()
            self._loop = (loop, thread)
        loop, _ = self._loop
        return asyncio.run_coroutine_threadsafe(coroutine, loop).result()

    def _loop_tasks(self) -> set:
        """The tasks pending on the plug's loop now: the driver's own (a
        reader, a keepalive), before its `__del__` can add a close."""
        if self._loop is None:
            return set()
        import asyncio

        loop, _ = self._loop

        async def pending():
            return {t for t in asyncio.all_tasks() if t is not asyncio.current_task()}

        try:
            return asyncio.run_coroutine_threadsafe(pending(), loop).result(timeout=1.0)
        except Exception:
            return set()

    def _cancel_loop_tasks(self, old: set, grace: Optional[float]):
        """On the still-running loop: cancel the tasks of `old` at once; a
        task started since (a close `__del__` scheduled) gets `grace`
        seconds to finish, then is cancelled. `grace=None` leaves it."""
        if self._loop is None:
            return
        import asyncio

        loop, _ = self._loop

        async def wind_down():
            tasks = [t for t in asyncio.all_tasks() if t is not asyncio.current_task()]
            stale = [t for t in tasks if t in old]
            fresh = [t for t in tasks if t not in old] if grace is not None else []
            for task in stale:
                task.cancel()
            await asyncio.gather(*stale, return_exceptions=True)
            if fresh and grace > 0:
                _, fresh = await asyncio.wait(fresh, timeout=grace)
            for task in fresh:
                task.cancel()
            await asyncio.gather(*fresh, return_exceptions=True)

        try:
            asyncio.run_coroutine_threadsafe(wind_down(), loop).result(
                timeout=(grace or 0) + 2.0
            )
        except Exception:
            pass

    def _stop_loop(self, old: set = frozenset()):
        """Cancel what the driver left running, then close the loop: a task
        dropped pending is an ERROR line on every run."""
        if self._loop is None:
            return
        import asyncio

        self._cancel_loop_tasks(old, grace=0.5)
        loop, thread = self._loop
        self._loop = None
        try:
            asyncio.run_coroutine_threadsafe(loop.shutdown_asyncgens(), loop).result(timeout=1.0)
        except Exception:
            pass
        loop.call_soon_threadsafe(loop.stop)
        thread.join(timeout=1.0)
        if not thread.is_alive():
            loop.close()

    def _cleanup(self) -> Dict[str, Any]:
        cleanup_start = time.time()

        self.logs.debug(f"Plug '{self.display_name}' starting cleanup")

        # False when a method still running kept the instance alive: its
        # `__del__` did not run (the force kill reports `skipped`).
        finalized = True
        if self.plug_instance:
            # Rust allows 5 s for Cleanup. Wait up to 3 s for an in-flight
            # method so dropping the reference really runs __del__; past
            # that, the running method keeps its bound reference and the
            # process exits (shutdown below) before __del__ can run.
            acquired = self._call_lock.acquire(timeout=3.0)
            # Said only to a class that has a finalizer to defer.
            has_finalizer = hasattr(type(self.plug_instance), "__del__")
            try:
                alive = weakref.ref(self.plug_instance)
            except TypeError:
                alive = None
            # The driver's own tasks, told apart from a close its `__del__`
            # schedules: those are cancelled at once, the close is awaited.
            driver_tasks = self._loop_tasks() if acquired else set()
            try:
                self.logs.debug(f"  -> Deleting plug instance (will call __del__)")
                del self.plug_instance
                self.plug_instance = None
                gc.collect()
                # A task the driver started (a reader) may still hold it:
                # cancelled on the running loop, so `__del__` then runs
                # while the loop still does and can schedule its close.
                if acquired and driver_tasks:
                    self._cancel_loop_tasks(driver_tasks, grace=None)
                    # A done task keeps its coroutine, and so the driver.
                    driver_tasks.clear()
                    gc.collect()
            finally:
                if acquired:
                    self._call_lock.release()
            # After `__del__`, which may have scheduled a close on it; not
            # while a method still runs on it.
            if acquired:
                self._stop_loop(driver_tasks)
                gc.collect()
            if acquired and has_finalizer and alive is not None and alive() is not None:
                finalized = False
            elif acquired:
                self.logs.debug(f"  -> Garbage collection completed (__del__ has finished)")
            elif has_finalizer:
                finalized = False
                # sys.stderr is the WARNING-level capture stream Rust reads.
                print(
                    f"Plug '{self.display_name}': a method is still running; "
                    "__del__ will not run before the plug process exits",
                    file=sys.stderr,
                    flush=True,
                )

        cleanup_duration = time.time() - cleanup_start
        self.logs.debug(
            f"Plug '{self.display_name}' cleanup complete ({cleanup_duration:.2f}s)"
        )

        if self._initialized:
            self.shutdown_event.set()

        return {
            "success": True,
            "message": f"Plug {self.display_name} cleaned up",
            "cleanup_duration_seconds": cleanup_duration,
            "finalized": finalized,
        }

    def _handle_shutdown(self) -> Dict[str, Any]:
        self.logs.debug(f"Plug '{self.display_name}' shutting down")
        self.shutdown_event.set()
        return {"success": True, "message": "Service shutting down"}


def handle_connection(conn: socket.socket, handler: PlugHandler):
    """Handle a single TCP connection: read request, write response."""
    try:
        # A client that connects and never sends must not pin a thread.
        conn.settimeout(30.0)
        buf = b""
        while b"\n" not in buf:
            chunk = conn.recv(65536)
            if not chunk:
                return
            buf += chunk
        conn.settimeout(None)

        line, _ = buf.split(b"\n", 1)
        request = json.loads(line.decode("utf-8"))

        response = handler.handle_request(request, conn)
        conn.sendall((json.dumps(response) + "\n").encode("utf-8"))

    except Exception as e:
        _original_stderr.write(f"Connection handler error: {e}\n")
        _original_stderr.flush()
        try:
            error_resp = {"success": False, "error": str(e)}
            conn.sendall((json.dumps(error_resp) + "\n").encode("utf-8"))
        except Exception:
            pass
    finally:
        conn.close()


def _start_parent_watchdog(on_parent_death, parent_pid: Optional[int] = None):
    """Exit when the spawning CLI process dies.

    The service is a TCP server with no stdin pipe to the parent, so a
    SIGKILLed parent (crash, force-quit, watchdog) otherwise leaves it
    alive forever, holding its listen socket and, through the plug
    instance, the instrument session the next run needs. Same mechanism
    as tp_worker.py. Unix: poll getppid(); the kernel reparents orphans
    to pid 1 (or a subreaper), so a change means the parent is gone.
    Windows: hold a SYNCHRONIZE handle to the parent and wait on it.

    `parent_pid` is the pid the spawner passed on the command line. It
    must not be read with getppid() here: if the parent died before this
    line ran, getppid() already returns the reaper's pid, the watchdog
    would take that as its reference and never fire (TP-1168). The
    getppid() fallback is for callers that predate `--parent-pid`.

    `on_parent_death` runs on the watchdog thread, or inline on the
    caller's thread when the parent is already gone, and must end the
    process itself.
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
        # Both "already dead" checks run on the caller's thread, like the
        # getppid() check below on Unix: the caller prints the port right
        # after this returns, and a parent that is gone must never be
        # announced to. A pid with no process object behind it fails
        # OpenProcess with ERROR_INVALID_PARAMETER (nothing held a handle,
        # the pid is free); an object already signalled is a parent that
        # exited while something still holds it. Both mean dead.
        handle = kernel32.OpenProcess(SYNCHRONIZE, False, parent_pid)
        if not handle:
            err = ctypes.get_last_error()
            if err == ERROR_INVALID_PARAMETER:
                on_parent_death()
                return
            # Any other failure (access denied, most plausibly) is not
            # evidence of death. Keep serving, unwatched, as before, but
            # say so: a silent disarm is what hid the original bug.
            print(
                f"parent watchdog not armed: OpenProcess({parent_pid}) failed with Win32 error {err}",
                file=_original_stderr,
                flush=True,
            )
            return
        if kernel32.WaitForSingleObject(handle, 0) == WAIT_OBJECT_0:
            on_parent_death()
            return

        def wait_windows():
            rc = kernel32.WaitForSingleObject(handle, INFINITE)
            if rc == WAIT_OBJECT_0:
                on_parent_death()
                return
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
        on_parent_death()
        return

    def poll_unix():
        while True:
            if os.getppid() != parent_pid:
                on_parent_death()
                return
            time.sleep(1)

    threading.Thread(target=poll_unix, daemon=True).start()


# Seconds an orphan may spend releasing its instrument before it exits
# regardless. Same budget Rust gives the Cleanup RPC.
ORPHAN_CLEANUP_SECONDS = 5.0


def _orphan_exit(handler: "PlugHandler", server: socket.socket):
    """Release the instrument and exit once the parent is gone.

    Mirrors the engine's own stop sequence (Cleanup, then Shutdown): the
    Cleanup drops the plug instance so its __del__ closes the instrument
    session. Nothing is left to answer an RPC, so the process exits
    directly instead of unwinding serve().

    The Cleanup runs __del__ inline on this thread, and nothing bounds
    it here the way Rust bounds the RPC: an instrument close blocking on
    a dead socket would keep the orphan alive for good, holding the
    session the next run needs. The timer is the bound.

    stderr is a pipe to the dead parent, so every write in here can
    raise BrokenPipeError (the log stream included): the exit must not
    depend on any of them.
    """
    timer = threading.Timer(ORPHAN_CLEANUP_SECONDS, lambda: os._exit(0))
    timer.daemon = True
    timer.start()
    try:
        try:
            print(
                f"Plug '{handler.display_name}': parent process gone, cleaning up and exiting",
                file=_original_stderr,
                flush=True,
            )
        except OSError:
            pass
        try:
            handler._handle_cleanup()
        except Exception:
            pass
        handler.shutdown_event.set()
        try:
            server.close()
        except OSError:
            pass
    finally:
        os._exit(0)


def serve(
    procedure_dir: Path,
    plug_key: str,
    display_name: str,
    plug_config: Dict[str, Any],
    parent_pid: Optional[int] = None,
):
    """Start NDJSON TCP plug service"""
    original_stdout = sys.stdout

    handler = PlugHandler(procedure_dir, plug_key, display_name, plug_config)

    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", 0))
    port = server.getsockname()[1]
    # Every worker opens one connection per call. With N slots in flight
    # the connects arrive in a burst; with the old `listen(5)` the overflow
    # was reset (macOS, Windows: ECONNRESET / broken pipe) or stalled on
    # SYN retransmits (Linux). The backlog is a safety margin only:
    # connections are accepted immediately into a thread and queue on the
    # handler's call lock, not in the kernel.
    server.listen(min(1024, socket.SOMAXCONN))

    # Armed before the port line: printing it is what releases the
    # parent, so from that point on the parent may die at any moment.
    # tp_worker.py orders it the same way.
    _start_parent_watchdog(lambda: _orphan_exit(handler, server), parent_pid)

    # Print port for Rust to discover
    print(f"NDJSON_PORT:{port}", flush=True, file=original_stdout)
    # The engine stops reading stdout after the port line. Anything that
    # writes to fd 1 later (a C library, a subprocess inheriting it)
    # would hit a dead pipe: send it to stderr, which is read.
    try:
        os.dup2(2, 1)
    except OSError:
        pass


    shutdown_event = handler.shutdown_event

    try:
        while not shutdown_event.is_set():
            server.settimeout(1.0)
            try:
                conn, addr = server.accept()
            except socket.timeout:
                continue

            # One thread per connection: accept never waits on a running
            # method. Method calls serialize on handler._call_lock.
            threading.Thread(
                target=handle_connection, args=(conn, handler), daemon=True
            ).start()
    except KeyboardInterrupt:
        print(f"Plug '{display_name}' interrupted", file=_original_stderr)
    finally:
        server.close()
        # Interpreter exit freezes daemon threads. Give a Cleanup that is
        # still running __del__ a bounded chance to finish first; Rust
        # kills the process a few seconds after Shutdown anyway.
        if handler.cleanup_started.is_set():
            handler.cleanup_done.wait(3.0)


if __name__ == "__main__":
    import argparse

    try:
        parser = argparse.ArgumentParser(description="Per-Plug NDJSON TCP Service")
        parser.add_argument(
            "--procedure-dir", required=True, help="Procedure directory"
        )
        parser.add_argument("--plug-name", required=True, help="Plug key")
        parser.add_argument("--display-name", required=True, help="Plug display name")
        # The engine passes the config in the environment (its `with:` may
        # hold secrets, and a command line is readable by every user of
        # the machine); `--plug-config` is kept for older callers.
        config_source = parser.add_mutually_exclusive_group(required=True)
        config_source.add_argument("--plug-config", help="Plug configuration (JSON)")
        config_source.add_argument(
            "--plug-config-env",
            help="Name of the environment variable holding the plug configuration (JSON)",
        )
        parser.add_argument(
            "--parent-pid",
            type=int,
            default=None,
            help="Pid of the spawning process; the service exits when it dies",
        )

        args = parser.parse_args()

        procedure_dir = Path(args.procedure_dir)
        if not procedure_dir.exists():
            print(
                f"Error: Procedure directory not found: {procedure_dir}",
                file=sys.stderr,
                flush=True,
            )
            sys.exit(1)

        if args.plug_config_env:
            # Removed before any plug code runs: a subprocess the plug
            # starts must not inherit it.
            raw_config = os.environ.pop(args.plug_config_env, None)
            if raw_config is None:
                print(
                    f"Error: {args.plug_config_env} is not set",
                    file=sys.stderr,
                    flush=True,
                )
                sys.exit(1)
        else:
            raw_config = args.plug_config
        try:
            plug_config = json.loads(raw_config)
        except json.JSONDecodeError as e:
            print(f"Error: Invalid plug config JSON: {e}", file=sys.stderr, flush=True)
            sys.exit(1)

        serve(
            procedure_dir,
            args.plug_name,
            args.display_name,
            plug_config,
            parent_pid=args.parent_pid,
        )
    except Exception as e:
        print(f"FATAL ERROR in tp_plug.py: {e}", file=sys.stderr, flush=True)
        import traceback

        traceback.print_exc(file=sys.stderr)
        sys.exit(1)
