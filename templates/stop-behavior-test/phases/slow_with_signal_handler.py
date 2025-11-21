import time
import signal

stop_requested = False


def signal_handler(signum, frame):
    global stop_requested
    print(f"[SIGNAL] Received signal {signum}, stopping gracefully...")
    stop_requested = True


def slow_with_signal_handler(test_api, ui):
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
