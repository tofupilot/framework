import sys
import time


def shutdown_system(test_api, ui):
    print("Shutting down test system (runs once)...", file=sys.stderr)
    time.sleep(0.5)
    print("Test system shut down", file=sys.stderr)
    return "CONTINUE"
