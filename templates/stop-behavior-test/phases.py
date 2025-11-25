import time
import signal
import sys
import socket
import threading
import subprocess

stop_requested = False


def signal_handler(signum, frame):
    global stop_requested
    print(f"[SIGNAL] Received signal {signum}, stopping gracefully...", file=sys.stderr)
    stop_requested = True


def setup_procedure(run, ui):
    """Setup procedure - initialize the test system"""
    print("🚀 [SETUP_PROCEDURE] Initializing test system...", file=sys.stderr)
    print("   - Starting test equipment", file=sys.stderr)
    print("   - Loading configuration", file=sys.stderr)
    time.sleep(1)
    print("✅ [SETUP_PROCEDURE] Test system initialized", file=sys.stderr)


def setup_slot(run, ui):
    """Setup slot - prepare each unit for testing"""
    print("🔧 [SETUP_SLOT] Preparing unit for test...", file=sys.stderr)
    print("   - Connecting to unit", file=sys.stderr)
    print("   - Running diagnostics", file=sys.stderr)
    time.sleep(1)
    print("✅ [SETUP_SLOT] Unit preparation complete", file=sys.stderr)


def cleanup_slot(run, ui):
    """Cleanup slot - clean up after each unit (SHOULD RUN EVEN WHEN STOPPED)"""
    print("🧹 [CLEANUP_SLOT] Finalizing unit test...", file=sys.stderr)
    print("   - Saving test data", file=sys.stderr)
    print("   - Disconnecting from unit", file=sys.stderr)
    print("   - Resetting for next unit", file=sys.stderr)
    time.sleep(5)
    print("✅ [CLEANUP_SLOT] Unit test finalized", file=sys.stderr)


def cleanup_procedure(run, ui):
    """Cleanup procedure - shutdown test system (SHOULD RUN EVEN WHEN STOPPED)"""
    print("🛑 [CLEANUP_PROCEDURE] Shutting down test system...", file=sys.stderr)
    print("   - Saving session data", file=sys.stderr)
    print("   - Powering down equipment", file=sys.stderr)
    print("   - Cleaning up resources", file=sys.stderr)
    time.sleep(5)
    print("✅ [CLEANUP_PROCEDURE] Test system shutdown complete", file=sys.stderr)


def quick_phase(run, ui):
    """Quick phase that completes in 1 second"""
    print("[Quick Phase] Starting (1 second)")
    time.sleep(1)
    print("[Quick Phase] Completed")


def slow_with_signal_handler(run, ui):
    """Slow phase that handles SIGTERM gracefully - THIS IS THE CORRECT PATTERN"""
    global stop_requested
    stop_requested = False

    signal.signal(signal.SIGTERM, signal_handler)
    signal.signal(signal.SIGINT, signal_handler)

    print("[Slow With Signal] Starting (30 seconds with signal handling)")

    for i in range(15):
        if stop_requested:
            print(
                f"[Slow With Signal] Stop requested at iteration {i}, exiting gracefully"
            )
            return

        print(f"[Slow With Signal] Running... {i+1}/30")
        time.sleep(2)

    print("[Slow With Signal] Completed all 30 iterations")


def normal_phase(run, ui):
    """Normal phase that takes 5 seconds"""
    print("[Normal Phase] Starting (5 seconds)")
    for i in range(5):
        print(f"[Normal Phase] Running... {i+1}/5")
        time.sleep(1)
    print("[Normal Phase] Completed")


def blocking_sleep_phase(run, ui):
    """Long sleep without signal handling - will not respond to SIGTERM gracefully"""
    print("[Blocking Sleep] Starting 60 second sleep WITHOUT signal handling")
    print(
        "[Blocking Sleep] Try stopping this - it should be force killed after timeout"
    )
    time.sleep(60)
    print("[Blocking Sleep] Completed (this shouldn't print if stopped)")


def cpu_loop_phase(run, ui):
    """Tight CPU loop without signal checks - will not respond to SIGTERM"""
    print("[CPU Loop] Starting CPU-intensive calculation")
    print("[CPU Loop] Try stopping this - it won't respond until loop completes")
    result = 0
    for i in range(10**9):
        result += i * 2
    print(f"[CPU Loop] Completed with result={result}")


def blocking_input_phase(run, ui):
    """Waiting for stdin input - will block on input()"""
    print("[Blocking Input] Waiting for user input from stdin")
    print("[Blocking Input] Try stopping this - input() blocks SIGTERM")
    try:
        response = input("Enter value: ")
        measurements.input_length = len(response)
        print(f"[Blocking Input] Received: {response}")
    except EOFError:
        print("[Blocking Input] Input interrupted")


def network_io_phase(run, ui):
    """Blocking network request - will block during HTTP request"""
    print("[Network I/O] Starting network request with 30 second delay")
    print("[Network I/O] Try stopping this - urlopen() blocks SIGTERM")
    try:
        import urllib.request

        response = urllib.request.urlopen("https://httpbin.org/delay/30", timeout=60)
        data = response.read()
        print(f"[Network I/O] Received {len(data)} bytes")
    except Exception as e:
        print(f"[Network I/O] Error: {e}")


def subprocess_blocking_phase(run, ui):
    """Spawned subprocess - parent blocks while subprocess runs"""
    print("[Subprocess] Spawning 'sleep 60' subprocess")
    print("[Subprocess] Try stopping this - subprocess.run() blocks parent")
    try:
        subprocess.run(["sleep", "60"], check=True)
        print("[Subprocess] Subprocess completed")
    except Exception as e:
        print(f"[Subprocess] Error: {e}")


def thread_wait_phase(run, ui):
    """Thread synchronization wait - blocks on Event.wait()"""
    print("[Thread Wait] Starting thread event wait (60 seconds)")
    print("[Thread Wait] Try stopping this - Event.wait() blocks SIGTERM")
    event = threading.Event()
    result = event.wait(timeout=60)
    if result:
        print("[Thread Wait] Event was set")
    else:
        print("[Thread Wait] Timeout reached")


def socket_server_phase(run, ui):
    """TCP socket waiting for connection - blocks on accept()"""
    print("[Socket Server] Creating TCP socket and waiting for connection")
    print("[Socket Server] Try stopping this - socket.accept() blocks SIGTERM")
    try:
        sock = socket.socket()
        sock.bind(("127.0.0.1", 0))
        sock.listen(1)
        port = sock.getsockname()[1]
        print(f"[Socket Server] Listening on port {port}")
        conn, addr = sock.accept()  # Blocks here
        print(f"[Socket Server] Connection from {addr}")
        conn.close()
        sock.close()
    except Exception as e:
        print(f"[Socket Server] Error: {e}")


def database_query_phase(run, ui):
    """Long-running database operation"""
    print("[Database Query] Starting long-running SQLite query")
    print("[Database Query] Try stopping this - database query blocks SIGTERM")
    try:
        import sqlite3

        conn = sqlite3.connect(":memory:")
        cursor = conn.cursor()
        cursor.execute(
            "WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM cnt LIMIT 10000000) SELECT COUNT(*) FROM cnt"
        )
        result = cursor.fetchone()
        print(f"[Database Query] Query result: {result}")
        conn.close()
    except Exception as e:
        print(f"[Database Query] Error: {e}")


def file_io_large_phase(run, ui):
    """Large file I/O operation"""
    print("[File I/O] Reading large amount of data from /dev/zero")
    print("[File I/O] Try stopping this - file read() blocks SIGTERM")
    try:
        with open("/dev/zero", "rb") as f:
            data = f.read(10**8)  # Read 100MB
        print(f"[File I/O] Read {len(data)} bytes")
    except Exception as e:
        print(f"[File I/O] Error: {e}")


def spawn_zombies_phase(run, ui):
    """Spawn multiple child processes to test zombie cleanup"""
    print("[Spawn Zombies] Creating 3 long-running child processes")
    print("[Spawn Zombies] These should be cleaned up automatically when worker exits")

    children = []
    for i in range(3):
        proc = subprocess.Popen(["sleep", "300"])
        children.append(proc)
        print(f"[Spawn Zombies] Spawned child process {proc.pid}")

    print("[Spawn Zombies] Phase complete, children still running")
    print("[Spawn Zombies] Check with 'ps aux | grep sleep' to see them")
    print("[Spawn Zombies] They should be killed when worker is stopped")
