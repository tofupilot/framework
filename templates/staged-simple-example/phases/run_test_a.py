import sys
import time


def run_test_a(test_api, ui):
    print("Running test scenario A...", file=sys.stderr)
    time.sleep(0.5)
    print("Test A completed", file=sys.stderr)
    return "CONTINUE"
