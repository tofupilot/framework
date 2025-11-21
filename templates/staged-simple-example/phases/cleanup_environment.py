import sys
import time


def cleanup_environment(test_api, ui):
    print("Cleaning up environment...", file=sys.stderr)
    time.sleep(0.5)
    print("Environment cleaned up", file=sys.stderr)
    return "CONTINUE"
